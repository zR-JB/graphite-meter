//! The controller's routes under `/login` and `/auth/`: the sign-in page, password sign-in, the session report and
//! sign-out; and the ticket mint.

use super::{
    AuthLease, Enabled, LOGIN_LIFETIME, Via, page,
    password::Password,
    policy::{SESSION_COOKIE, cookie},
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
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use serde::Serialize;
use std::{
    future::poll_fn,
    pin::pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// A controller route reads at most this many body bytes.
const FORM_BYTES: usize = 4096;
const LOGIN_COOKIE: &str = "__Host-gm_login";
const CSRF_COOKIE: &str = "__Host-gm_csrf";
const DEVICE_COOKIE: &str = "__Host-gm_device";
/// A sign-in form's token lasts this long.
const FORM_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Answers a controller route the policy let through; a path no route claims is not found.
pub(super) async fn handle<B: http_body::Body>(auth: &Enabled, request: Request<B>, peer: &Peer) -> Response<Body> {
    let method = match request.method() {
        &Method::HEAD => &Method::GET,
        method => method,
    };
    let lease = peer.auth().filter(|lease| lease.via() == &Via::Cookie);
    let mut answer = match (method, request.uri().path(), &auth.password) {
        (&Method::GET, "/login", _) => login_page(auth, &request),
        (&Method::POST, "/auth/password", Some(password)) => sign_in(auth, password, request, peer).await,
        (&Method::GET, "/auth/session", _) => session(auth, lease),
        (&Method::POST, "/auth/logout", _) => {
            let from_public = request.headers().get(header::ORIGIN) == Some(&auth.policy.origin);
            let (head, body) = request.into_parts();
            let form = form(&head.headers, body).await.filter(|_| from_public);
            sign_out(auth, lease, form)
        }
        _ => response::status(StatusCode::NOT_FOUND),
    };
    protect(answer.headers_mut(), true);
    answer
}

/// A ticket for an HTTPS `target` route of `kind` on the public hostname, minted by a cookie login or a browser
/// grant.
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
    let password = if auth.password.is_some() { "true" } else { "" };
    let page = page::login(&[
        ("CSRF", &csrf),
        ("Provider", &auth.provider),
        ("Challenge", challenge),
        ("Notice", notice),
        ("Status", status),
        ("Password", password),
    ]);
    let mut answer = response::html(page);
    set_cookie(&mut answer, LOGIN_COOKIE, &csrf, SystemTime::now() + FORM_LIFETIME);
    answer
}

/// Go's password sign-in: the form's CSRF proof, the attempt budgets, a free verifier slot and the hash, then a
/// login replacing the one the request presented.
async fn sign_in<B: http_body::Body>(
    auth: &Enabled,
    password: &Password,
    request: Request<B>,
    peer: &Peer,
) -> Response<Body> {
    let (head, body) = request.into_parts();
    let headers = &head.headers;
    let Some(form) = form(headers, body).await else {
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
    let Some(login) = auth.store.sign_in("local-operator", "Local operator", "local") else {
        return rejected(auth, Reason::SessionCapacity, challenge);
    };
    if let Some(prior) = cookie(headers, SESSION_COOKIE).and_then(|token| auth.store.cookie(token)) {
        auth.store.sign_out(prior.login(), false);
    }
    auth.security.count(Counter::Local);
    let mut answer = redirect(&match verification_code(challenge) {
        Some(_) => format!("/auth/cli?challenge={challenge}"),
        None => "/".into(),
    });
    set_cookie(&mut answer, SESSION_COOKIE, &login.token, login.expires);
    set_cookie(&mut answer, CSRF_COOKIE, &login.csrf, login.expires);
    clear_cookie(&mut answer, LOGIN_COOKIE);
    let (device, expires) = password.device(SystemTime::now());
    set_cookie(&mut answer, DEVICE_COOKIE, &device, expires);
    answer
}

/// Go's check of a sign-in form: posted from the public origin with the token its login cookie holds.
fn check_csrf(auth: &Enabled, headers: &HeaderMap, proof: &str) -> Result<(), Reason> {
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
fn rejected(auth: &Enabled, reason: Reason, challenge: &str) -> Response<Body> {
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
fn sign_out(auth: &Enabled, lease: Option<&AuthLease>, form: Option<Vec<(String, String)>>) -> Response<Body> {
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
fn redirect(target: &str) -> Response<Body> {
    let target = HeaderValue::from_str(target).expect("targets are ASCII");
    response::redirect(&Method::POST, StatusCode::SEE_OTHER, &target)
}

/// Go's `setCookie`: host-only, secure and strictly same-site; only the CSRF cookie, which pages read, is not
/// HttpOnly.
fn set_cookie(answer: &mut Response<Body>, name: &str, value: &str, expires: SystemTime) {
    let age = expires.duration_since(SystemTime::now()).map_or(0, |age| age.as_secs());
    let http_only = if name == CSRF_COOKIE { "" } else { "; HttpOnly" };
    let date = http_date(expires);
    let cookie = format!("{name}={value}; Path=/; Expires={date}; Max-Age={age}{http_only}; Secure; SameSite=Strict");
    let cookie = HeaderValue::from_str(&cookie).expect("cookies are ASCII");
    answer.headers_mut().append(header::SET_COOKIE, cookie);
}

fn clear_cookie(answer: &mut Response<Body>, name: &str) {
    set_cookie(answer, name, "", UNIX_EPOCH + Duration::from_secs(1));
}

/// A form field's value; empty when absent.
fn field<'a>(form: &'a [(String, String)], name: &str) -> &'a str {
    form.iter()
        .find(|(key, _)| key == name)
        .map_or("", |(_, value)| value.as_str())
}

/// A URL-encoded form body of at most 4 KiB with unique, well-formed fields.
async fn form<B: http_body::Body>(headers: &HeaderMap, body: B) -> Option<Vec<(String, String)>> {
    let media = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
    let media = media.split(';').next().unwrap_or_default().trim();
    if !media.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        return None;
    }
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
    query::form(std::str::from_utf8(&bytes).ok()?)
}
