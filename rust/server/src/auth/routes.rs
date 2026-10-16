//! The controller's `/login` and `/auth/` routes: sign-in, password and OIDC, approvals, session, sign-out, tickets.

use super::{
    AuthLease, Counter, Enabled, Identity, LOGIN_LIFETIME, LoginKey, Principal, Reason, Via,
    approval::{self, Challenge},
    oidc::{self, Oidc},
    page,
    password::{Password, Verdict},
    policy::{AuthRoute, Cookie, cookie, cookie_lease},
    protect,
    store::{MintRefusal, random},
};
use crate::{
    app::{query, response},
    clock::{http_date, rfc3339},
    limits::Refusal,
    peer::Peer,
    transport::body::Body,
};
use bytes::{Buf, BufMut};
use graphite_meter_proto::{
    origin::{Origin, Scheme},
    route::{Kind, Route},
};
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header, request::Parts};
use serde::Serialize;
use std::{
    future::poll_fn,
    pin::pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::time::Instant;
use zeroize::Zeroizing;

/// A controller route reads at most this many body bytes.
const FORM_BYTES: usize = 4096;
/// A sign-in form's token lasts this long.
const FORM_LIFETIME: Duration = Duration::from_mins(10);

/// A form's fields in order, zeroized when dropped.
pub(super) type Form = Zeroizing<Vec<(String, String)>>;

/// Answers a controller route the policy let through; a path no route claims is not found.
pub(super) async fn handle<B: http_body::Body>(
    auth: &Enabled,
    request: Request<B>,
    deadline: Instant,
    peer: &Peer,
) -> Response<Body> {
    // A HEAD reaches the GET route.
    let method = match request.method() {
        &Method::HEAD => Method::GET,
        method => method.clone(),
    };
    let route = AuthRoute::find(method.as_str(), request.uri().path());
    let lease = peer.auth().filter(|lease| lease.via() == &Via::Cookie);
    let from_public = request.headers().get(header::ORIGIN) == Some(&auth.policy.origin);
    let oidc = auth.sign_in.oidc();
    let mut answer = match (route, auth.sign_in.password(), oidc) {
        (Some(AuthRoute::LoginPage), ..) => login_page(auth, &request),
        (Some(AuthRoute::Password), Some(password), _) => sign_in(auth, password, request, peer).await,
        (Some(AuthRoute::OidcStart), _, Some(provider)) => oidc::start(auth, provider, request, peer).await,
        (Some(AuthRoute::OidcCallback), _, Some(provider)) => {
            oidc::callback(auth, provider, request.into_parts().0, deadline, peer).await
        }
        (Some(AuthRoute::Session), ..) => session(auth, lease),
        (Some(AuthRoute::CliPage), ..) => approval::cli_page(auth, &request, peer),
        (Some(AuthRoute::BrowserPage), ..) => approval::browser_page(auth, &request, peer),
        (Some(route @ (AuthRoute::CliApprove | AuthRoute::BrowserApprove)), ..) => {
            let form = form(request).await.1.filter(|_| from_public);
            approval::approve(auth, lease, form, route == AuthRoute::BrowserApprove)
        }
        (Some(route @ (AuthRoute::CliToken | AuthRoute::BrowserToken)), ..) => {
            approval::token(auth, request, route == AuthRoute::BrowserToken).await
        }
        (Some(AuthRoute::Logout), ..) => sign_out(auth, lease, form(request).await.1.filter(|_| from_public)),
        (Some(AuthRoute::Password | AuthRoute::OidcStart | AuthRoute::OidcCallback), ..) | (None, ..) => {
            response::status(StatusCode::NOT_FOUND)
        }
    };
    protect(answer.headers_mut(), true);
    if let Some(provider) = oidc.and_then(Oidc::provider)
        && matches!(route, Some(AuthRoute::LoginPage | AuthRoute::OidcStart))
    {
        page::allow_form_action(answer.headers_mut(), &provider.origin);
    }
    answer
}

/// A ticket for an HTTPS `target` route of `kind` on the public hostname, minted by a cookie login or a browser grant.
pub(super) fn mint<B>(auth: &Enabled, request: &Request<B>, lease: Option<&AuthLease>, kind: Kind) -> Response<Body> {
    let Some(lease) = lease.filter(|lease| lease.via() != &Via::Bearer(None)) else {
        return response::status(StatusCode::FORBIDDEN);
    };
    let target = query::get(request.uri().query(), "target").unwrap_or_default();
    let Some((origin, path)) = Origin::split(target.strip_suffix('#').unwrap_or(&target)).ok() else {
        return response::status(StatusCode::BAD_REQUEST);
    };
    let route = Route::find_by(Route::path, path).filter(|route| route.kind() == kind);
    if route.is_none() || origin.scheme != Scheme::Https || origin.host != auth.policy.public.host {
        return response::status(StatusCode::BAD_REQUEST);
    }
    let requester = request.headers().get(header::ORIGIN).cloned();
    match auth.store.mint(lease, format!("{origin}{path}"), requester) {
        Ok(ticket) => response::json_of(&ticket),
        Err(MintRefusal::Ended) => response::status(StatusCode::FORBIDDEN),
        Err(MintRefusal::Full) => response::busy(Refusal::Client),
    }
}

/// The sign-in page; its form proves a fresh token the login cookie holds for ten minutes.
fn login_page<B>(auth: &Enabled, request: &Request<B>) -> Response<Body> {
    let read = |name| query::get(request.uri().query(), name).unwrap_or_default();
    let (challenge, error, reason) = (Challenge::parse(&read("challenge")), read("error"), read("reason"));
    let notice = match error.as_str() {
        notice @ ("" | "provider" | "busy" | "stale" | "throttled" | "password" | "proxy") => notice,
        _ => "failed",
    };
    let status = match reason.as_str() {
        status @ ("expired" | "renew" | "signed_out") => status,
        _ => "",
    };
    let csrf = random::<32>();
    let oidc = auth.sign_in.oidc();
    let page = page::Login {
        csrf: &csrf,
        provider: &auth.provider,
        challenge: challenge.as_ref().map(Challenge::as_str),
        notice,
        status,
        password: auth.sign_in.password().is_some(),
        oidc: oidc.is_some(),
        oidc_ready: oidc.and_then(Oidc::provider).is_some(),
    }
    .render();
    let mut answer = response::html(page);
    set_cookie(&mut answer, Cookie::Login, &csrf, SystemTime::now() + FORM_LIFETIME);
    answer
}

/// Password sign-in: CSRF proof, attempt budgets, a free verifier slot and the hash, then a replacing login.
async fn sign_in<B: http_body::Body>(
    auth: &Enabled,
    password: &Password,
    request: Request<B>,
    peer: &Peer,
) -> Response<Body> {
    let (head, form) = form(request).await;
    let headers = &head.headers;
    let Some(form) = form else {
        return rejected(auth, Reason::MalformedForm, None);
    };
    let challenge = Challenge::parse(field(&form, "challenge"));
    let admitted = || match password.admit(peer.keys(), cookie(headers, Cookie::Device)) {
        true => Ok(()),
        false if peer.keys().is_none() => Err(Reason::ClientAddress),
        false => Err(Reason::Throttled),
    };
    if let Err(reason) = check_csrf(auth, headers, field(&form, "csrf")).and_then(|()| admitted()) {
        return rejected(auth, reason, challenge.as_ref());
    }
    let entered = Zeroizing::new(field(&form, "password").to_owned());
    match password.verify(entered).await {
        Verdict::Match => {}
        Verdict::Mismatch => return rejected(auth, Reason::PasswordMismatch, challenge.as_ref()),
        Verdict::Busy => return rejected(auth, Reason::VerifierBusy, challenge.as_ref()),
    }
    let target = match &challenge {
        Some(challenge) => format!("/auth/cli?challenge={}", challenge.as_str()),
        None => "/".into(),
    };
    let identity = Identity {
        principal: Principal::operator(),
        name: "Local operator",
        provider: "local",
    };
    let prior = cookie_lease(&auth.store, headers).map(|lease| lease.login());
    let mut answer = match establish(auth, identity, prior, redirect(&target)) {
        Ok(answer) => answer,
        Err(reason) => return rejected(auth, reason, challenge.as_ref()),
    };
    auth.security.count(Counter::Local);
    crate::log!(Info, "auth", "signed in: local operator, with the password");
    let (device, expires) = password.device(SystemTime::now());
    set_cookie(&mut answer, Cookie::Device, &device, expires);
    answer
}

/// Signs `identity` in, ending the `prior` login, and sets the login's cookies on `answer`.
pub(super) fn establish(
    auth: &Enabled,
    identity: Identity<'_>,
    prior: Option<LoginKey>,
    mut answer: Response<Body>,
) -> Result<Response<Body>, Reason> {
    let login = auth.store.sign_in(identity);
    let login = login.ok_or(Reason::SessionCapacity)?;
    if let Some(prior) = prior {
        auth.store.sign_out(prior, false);
    }
    set_cookie(&mut answer, Cookie::Session, login.token.as_str(), login.expires);
    set_cookie(&mut answer, Cookie::Csrf, login.csrf.as_str(), login.expires);
    clear_cookie(&mut answer, Cookie::Login);
    Ok(answer)
}

/// Checks a sign-in form: posted from the public origin with the token its login cookie holds.
pub(super) fn check_csrf(auth: &Enabled, headers: &HeaderMap, proof: &str) -> Result<(), Reason> {
    let origin = headers.get(header::ORIGIN).filter(|origin| !origin.is_empty());
    match (origin, cookie(headers, Cookie::Login)) {
        (None, _) => Err(Reason::CsrfOriginMissing),
        (Some(origin), _) if *origin != auth.policy.origin => Err(Reason::CsrfOriginMismatch),
        (_, None) => Err(Reason::CsrfCookieMissing),
        _ if proof.is_empty() => Err(Reason::CsrfTokenMissing),
        (_, Some(token)) if token.len() <= 20 || !bool::from(token.as_bytes().ct_eq(proof.as_bytes())) => {
            Err(Reason::CsrfTokenMismatch)
        }
        _ => Ok(()),
    }
}

/// Back to the sign-in page with the refusal's notice, keeping the approval challenge.
pub(super) fn rejected(auth: &Enabled, reason: Reason, challenge: Option<&Challenge>) -> Response<Body> {
    auth.security.refused(reason);
    let notice = reason.notice();
    redirect(&match challenge {
        Some(challenge) => format!("/login?challenge={}&error={notice}", challenge.as_str()),
        None => format!("/login?error={notice}"),
    })
}

/// The signed-in login's name, provider, expiry and CSRF token.
fn session(auth: &Enabled, lease: Option<&AuthLease>) -> Response<Body> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Report {
        name: String,
        provider: String,
        expires: String,
        csrf: String,
        remaining_ms: u128,
        maximum_lifetime_ms: u128,
    }
    let Some(login) = lease.and_then(|lease| auth.store.view(lease.login())) else {
        return response::empty(StatusCode::FORBIDDEN);
    };
    let remaining = login.expires.duration_since(SystemTime::now()).unwrap_or_default();
    response::json_of(&Report {
        name: login.name,
        provider: login.provider,
        expires: rfc3339(login.expires),
        csrf: login.csrf.as_str().to_owned(),
        remaining_ms: remaining.as_millis(),
        maximum_lifetime_ms: LOGIN_LIFETIME.as_millis(),
    })
}

/// Ends the login, or with `scope=all` every login of its subject, when the form proves its CSRF token.
fn sign_out(auth: &Enabled, lease: Option<&AuthLease>, form: Option<Form>) -> Response<Body> {
    let (Some(lease), Some(form)) = (lease, form) else {
        return response::empty(StatusCode::FORBIDDEN);
    };
    let (store, every) = (&auth.store, field(&form, "scope") == "all");
    if !store.csrf(lease.login(), field(&form, "csrf")) || !store.sign_out(lease.login(), every) {
        return response::empty(StatusCode::FORBIDDEN);
    }
    auth.security.count(Counter::Logout);
    let mut answer = redirect("/login?reason=signed_out");
    for cookie in [Cookie::Session, Cookie::Login, Cookie::Csrf] {
        clear_cookie(&mut answer, cookie);
    }
    answer
}

/// A 303 answering a form post.
pub(super) fn redirect(target: &str) -> Response<Body> {
    let target = HeaderValue::from_str(target).expect("targets are ASCII");
    response::redirect(&Method::POST, StatusCode::SEE_OTHER, &target)
}

/// Sets `cookie` to `value` until `expires`.
pub(super) fn set_cookie(answer: &mut Response<Body>, cookie: Cookie, value: &str, expires: SystemTime) {
    let age = expires.duration_since(SystemTime::now()).map_or(0, |age| age.as_secs());
    let (name, same_site) = (cookie.name(), cookie.same_site());
    let http_only = if cookie.script() { "" } else { "; HttpOnly" };
    let date = http_date(expires);
    let cookie =
        format!("{name}={value}; Path=/; Expires={date}; Max-Age={age}{http_only}; Secure; SameSite={same_site}");
    let cookie = HeaderValue::from_str(&cookie).expect("cookies are ASCII");
    answer.headers_mut().append(header::SET_COOKIE, cookie);
}

pub(super) fn clear_cookie(answer: &mut Response<Body>, cookie: Cookie) {
    set_cookie(answer, cookie, "", UNIX_EPOCH + Duration::from_secs(1));
}

/// A form field's value; empty when absent.
pub(super) fn field<'a>(form: &'a [(String, String)], name: &str) -> &'a str {
    let found = form.iter().find(|(key, _)| key == name);
    found.map_or("", |(_, value)| value.as_str())
}

/// The request's head, and its URL-encoded form body of at most 4 KiB with unique, well-formed fields.
pub(super) async fn form<B: http_body::Body>(request: Request<B>) -> (Parts, Option<Form>) {
    let (head, body) = request.into_parts();
    let content_type = head.headers.get(header::CONTENT_TYPE);
    let media = content_type.and_then(|media| media.to_str().ok());
    let media = media.unwrap_or_default().split(';').next().unwrap_or_default().trim();
    if !media.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        return (head, None);
    }
    let bytes = self::body(body).await;
    let form = bytes.and_then(|bytes| query::form(std::str::from_utf8(&bytes).ok()?).map(Zeroizing::new));
    (head, form)
}

/// A request body of at most 4 KiB, in one allocation zeroized when dropped.
pub(super) async fn body<B: http_body::Body>(body: B) -> Option<Zeroizing<Vec<u8>>> {
    let mut body = pin!(body);
    let mut bytes = Zeroizing::new(Vec::with_capacity(FORM_BYTES));
    while let Some(frame) = poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
        if let Ok(data) = frame.ok()?.into_data() {
            if bytes.len() + data.remaining() > FORM_BYTES {
                return None;
            }
            bytes.put(data);
        }
    }
    Some(bytes)
}
