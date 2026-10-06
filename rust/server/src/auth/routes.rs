//! The controller's `/login` and `/auth/` routes: sign-in, password and OIDC, approvals, session, sign-out, tickets.

use super::{
    AuthLease, Enabled, LOGIN_LIFETIME, LoginKey, Via, approval,
    oidc::{self, Oidc, TRANSACTION_COOKIE},
    page::{self, Page},
    password::Password,
    policy::{SESSION_COOKIE, cookie, cookie_lease},
    protect,
    security::{Counter, Reason},
    store::{MintRefusal, random},
};
use crate::{
    app::{query, response},
    log::{http_date, rfc3339},
    peer::Peer,
    transport::body::Body,
};
use bytes::{Buf, BufMut};
use graphite_meter_proto::{
    approval::verification_code,
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
const LOGIN_COOKIE: &str = "__Host-gm_login";
const CSRF_COOKIE: &str = "__Host-gm_csrf";
const DEVICE_COOKIE: &str = "__Host-gm_device";
/// A sign-in form's token lasts this long.
const FORM_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// A form's fields in order.
pub(super) type Form = Vec<(String, String)>;

/// Answers a controller route the policy let through; a path no route claims is not found.
pub(super) async fn handle<B: http_body::Body>(
    auth: &Enabled,
    request: Request<B>,
    deadline: Instant,
    peer: &Peer,
) -> Response<Body> {
    let method = match request.method() {
        &Method::HEAD => Method::GET,
        method => method.clone(),
    };
    let path = request.uri().path().to_owned();
    let lease = peer.auth().filter(|lease| lease.via() == &Via::Cookie);
    let from_public = request.headers().get(header::ORIGIN) == Some(&auth.policy.origin);
    let oidc = auth.oidc.as_ref();
    let mut answer = match (&method, path.as_str(), &auth.password, oidc) {
        (&Method::GET, "/login", ..) => login_page(auth, &request),
        (&Method::POST, "/auth/password", Some(password), _) => sign_in(auth, password, request, peer).await,
        (&Method::POST, "/auth/oidc/start", _, Some(provider)) => oidc::start(auth, provider, request, peer).await,
        (&Method::GET, "/auth/oidc/callback", _, Some(provider)) => {
            oidc::callback(auth, provider, request.into_parts().0, deadline, peer).await
        }
        (&Method::GET, "/auth/session", ..) => session(auth, lease),
        (&Method::GET, "/auth/cli", ..) => approval::cli_page(auth, &request, peer),
        (&Method::GET, "/auth/browser", ..) => approval::browser_page(auth, &request, peer),
        (&Method::POST, "/auth/cli/approve" | "/auth/browser/approve", ..) => {
            let form = form(request).await.1.filter(|_| from_public);
            approval::approve(auth, lease, form, path.starts_with("/auth/browser"))
        }
        (&Method::POST, "/auth/cli/token" | "/auth/browser/token", ..) => {
            approval::token(auth, request, path.starts_with("/auth/browser")).await
        }
        (&Method::POST, "/auth/logout", ..) => sign_out(auth, lease, form(request).await.1.filter(|_| from_public)),
        _ => response::status(StatusCode::NOT_FOUND),
    };
    protect(answer.headers_mut(), true);
    if let Some(provider) = oidc.and_then(Oidc::provider)
        && matches!(path.as_str(), "/login" | "/auth/oidc/start")
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
    let route = Route::from_path(path).filter(|route| route.kind() == kind);
    if route.is_none() || origin.scheme != Scheme::Https || origin.host != auth.policy.public.host {
        return response::status(StatusCode::BAD_REQUEST);
    }
    let requester = request.headers().get(header::ORIGIN).cloned();
    match auth.store.mint(lease, format!("{origin}{path}"), requester) {
        Ok(ticket) => response::json_of(&ticket),
        Err(MintRefusal::Ended) => response::status(StatusCode::FORBIDDEN),
        Err(MintRefusal::Full) => {
            let mut busy = response::status(StatusCode::TOO_MANY_REQUESTS);
            busy.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            busy
        }
    }
}

/// The sign-in page; its form proves a fresh token the login cookie holds for ten minutes.
fn login_page<B>(auth: &Enabled, request: &Request<B>) -> Response<Body> {
    let read = |name| query::get(request.uri().query(), name).unwrap_or_default();
    let (challenge, error, reason) = (read("challenge"), read("error"), read("reason"));
    let challenge = if verification_code(&challenge).is_some() { challenge.as_str() } else { "" };
    let notice = match error.as_str() {
        notice @ ("" | "provider" | "busy" | "stale" | "throttled" | "password") => notice,
        _ => "failed",
    };
    let status = match reason.as_str() {
        status @ ("expired" | "renew" | "signed_out") => status,
        _ => "",
    };
    let csrf = random::<32>();
    let flag = |on: bool| if on { "true" } else { "" };
    let oidc = auth.oidc.as_ref();
    let page = page::render(
        Page::Login,
        &[
            ("CSRF", &csrf),
            ("Provider", &auth.provider),
            ("Challenge", challenge),
            ("Notice", notice),
            ("Status", status),
            ("Password", flag(auth.password.is_some())),
            ("OIDC", flag(oidc.is_some())),
            ("OIDCReady", flag(oidc.and_then(Oidc::provider).is_some())),
        ],
    );
    let mut answer = response::html(page);
    set_cookie(&mut answer, LOGIN_COOKIE, &csrf, SystemTime::now() + FORM_LIFETIME);
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
        return rejected(auth, Reason::MalformedForm, "");
    };
    let challenge = field(&form, "challenge");
    let admitted = || match password.admit(peer.keys(), cookie(headers, DEVICE_COOKIE)) {
        true => Ok(()),
        false => Err(Reason::Throttled),
    };
    if let Err(reason) = check_csrf(auth, headers, field(&form, "csrf")).and_then(|()| admitted()) {
        return rejected(auth, reason, challenge);
    }
    match password
        .verify(Zeroizing::new(field(&form, "password").to_owned()))
        .await
    {
        Some(true) => {}
        Some(false) => return rejected(auth, Reason::PasswordMismatch, challenge),
        None => return rejected(auth, Reason::VerifierBusy, challenge),
    }
    let target = match verification_code(challenge) {
        Some(_) => format!("/auth/cli?challenge={challenge}"),
        None => "/".into(),
    };
    let identity = ("local-operator", "Local operator", "local");
    let prior = cookie_lease(&auth.store, headers).map(|lease| lease.login());
    let mut answer = match establish(auth, identity, prior, redirect(&target)) {
        Ok(answer) => answer,
        Err(reason) => return rejected(auth, reason, challenge),
    };
    auth.security.count(Counter::Local);
    let (device, expires) = password.device(SystemTime::now());
    set_cookie(&mut answer, DEVICE_COOKIE, &device, expires);
    answer
}

/// Signs `(subject, name, provider)` in, ending the `prior` login, and sets the login's cookies on `answer`.
pub(super) fn establish(
    auth: &Enabled,
    (subject, name, provider): (&str, &str, &str),
    prior: Option<LoginKey>,
    mut answer: Response<Body>,
) -> Result<Response<Body>, Reason> {
    let login = auth.store.sign_in(subject, name, provider);
    let login = login.ok_or(Reason::SessionCapacity)?;
    if let Some(prior) = prior {
        auth.store.sign_out(prior, false);
    }
    set_cookie(&mut answer, SESSION_COOKIE, &login.token, login.expires);
    set_cookie(&mut answer, CSRF_COOKIE, &login.csrf, login.expires);
    clear_cookie(&mut answer, LOGIN_COOKIE);
    Ok(answer)
}

/// Checks a sign-in form: posted from the public origin with the token its login cookie holds.
pub(super) fn check_csrf(auth: &Enabled, headers: &HeaderMap, proof: &str) -> Result<(), Reason> {
    let origin = headers.get(header::ORIGIN).filter(|origin| !origin.is_empty());
    match (origin, cookie(headers, LOGIN_COOKIE)) {
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

/// Back to the sign-in page with the refusal's notice, keeping a valid approval challenge.
pub(super) fn rejected(auth: &Enabled, reason: Reason, challenge: &str) -> Response<Body> {
    auth.security.refused(reason);
    let notice = reason.notice();
    redirect(&match verification_code(challenge) {
        Some(_) => format!("/login?challenge={challenge}&error={notice}"),
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
        csrf: login.csrf,
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
    for name in [SESSION_COOKIE, LOGIN_COOKIE, CSRF_COOKIE] {
        clear_cookie(&mut answer, name);
    }
    answer
}

/// A 303 answering a form post.
pub(super) fn redirect(target: &str) -> Response<Body> {
    let target = HeaderValue::from_str(target).expect("targets are ASCII");
    response::redirect(&Method::POST, StatusCode::SEE_OTHER, &target)
}

/// Sets a host-only secure cookie, HttpOnly but for CSRF, strictly same-site but for the OIDC transaction's.
pub(super) fn set_cookie(answer: &mut Response<Body>, name: &str, value: &str, expires: SystemTime) {
    let age = expires.duration_since(SystemTime::now()).map_or(0, |age| age.as_secs());
    let http_only = if name == CSRF_COOKIE { "" } else { "; HttpOnly" };
    let same_site = if name == TRANSACTION_COOKIE { "Lax" } else { "Strict" };
    let date = http_date(expires);
    let cookie =
        format!("{name}={value}; Path=/; Expires={date}; Max-Age={age}{http_only}; Secure; SameSite={same_site}");
    let cookie = HeaderValue::from_str(&cookie).expect("cookies are ASCII");
    answer.headers_mut().append(header::SET_COOKIE, cookie);
}

pub(super) fn clear_cookie(answer: &mut Response<Body>, name: &str) {
    set_cookie(answer, name, "", UNIX_EPOCH + Duration::from_secs(1));
}

/// A form field's value; empty when absent.
pub(super) fn field<'a>(form: &'a [(String, String)], name: &str) -> &'a str {
    form.iter()
        .find(|(key, _)| key == name)
        .map_or("", |(_, value)| value.as_str())
}

/// The request's head, and its URL-encoded form body of at most 4 KiB with unique, well-formed fields.
pub(super) async fn form<B: http_body::Body>(request: Request<B>) -> (Parts, Option<Form>) {
    let (head, body) = request.into_parts();
    let media = head
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|media| media.to_str().ok());
    let media = media.unwrap_or_default().split(';').next().unwrap_or_default().trim();
    if !media.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        return (head, None);
    }
    let form = self::body(body)
        .await
        .and_then(|bytes| query::form(std::str::from_utf8(&bytes).ok()?));
    (head, form)
}

/// A request body of at most 4 KiB.
pub(super) async fn body<B: http_body::Body>(body: B) -> Option<Vec<u8>> {
    let mut body = pin!(body);
    let mut bytes = Vec::new();
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
