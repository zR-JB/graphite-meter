//! The controller's routes under `/login` and `/auth/` (the session report and sign-out), and the ticket mint.

use super::{AuthLease, LOGIN_LIFETIME, Policy, Store, Via, protect, store::MintRefusal};
use crate::{
    app::{query, response},
    log::rfc3339,
    transport::body::Body,
};
use bytes::{Buf, BufMut};
use graphite_meter_proto::{
    origin::{Origin, Scheme},
    route::{Kind, Route},
};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use serde::Serialize;
use std::{future::poll_fn, pin::pin, time::SystemTime};

/// A controller route reads at most this many body bytes.
const FORM_BYTES: usize = 4096;

/// Answers a controller route the policy let through; a path no route claims is not found.
pub(super) async fn handle<B: http_body::Body>(
    policy: &Policy,
    store: &Store,
    request: Request<B>,
    lease: Option<&AuthLease>,
) -> Response<Body> {
    let method = match request.method() {
        &Method::HEAD => &Method::GET,
        method => method,
    };
    let lease = lease.filter(|lease| lease.via() == &Via::Cookie);
    let mut answer = match (method, request.uri().path()) {
        (&Method::GET, "/auth/session") => session(store, lease),
        (&Method::POST, "/auth/logout") => {
            let from_public = request.headers().get(header::ORIGIN) == Some(&policy.origin);
            let form = form(request).await.filter(|_| from_public);
            sign_out(store, lease, form)
        }
        _ => response::status(StatusCode::NOT_FOUND),
    };
    protect(answer.headers_mut(), true);
    answer
}

/// A ticket for an HTTPS `target` route of `kind` on the public hostname, minted by a cookie login or a browser
/// grant.
pub(super) fn mint<B>(
    policy: &Policy,
    store: &Store,
    request: &Request<B>,
    lease: Option<&AuthLease>,
    kind: Kind,
) -> Response<Body> {
    let Some(lease) = lease.filter(|lease| lease.via() != &Via::Bearer(None)) else {
        return response::status(StatusCode::FORBIDDEN);
    };
    let target = query::get(request.uri().query(), "target").unwrap_or_default();
    let Some((origin, path)) = Origin::split(target.strip_suffix('#').unwrap_or(&target)).ok() else {
        return response::status(StatusCode::BAD_REQUEST);
    };
    let route = Route::from_path(path).filter(|route| route.kind() == kind);
    if route.is_none() || origin.scheme != Scheme::Https || origin.host != policy.public.host {
        return response::status(StatusCode::BAD_REQUEST);
    }
    match store.mint(lease, format!("{origin}{path}"), request.headers().get(header::ORIGIN).cloned()) {
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

/// The signed-in login's name, provider, expiry and CSRF token.
fn session(store: &Store, lease: Option<&AuthLease>) -> Response<Body> {
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
    let Some(login) = lease.and_then(|lease| store.view(lease.login())) else {
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
fn sign_out(store: &Store, lease: Option<&AuthLease>, form: Option<Vec<(String, String)>>) -> Response<Body> {
    let (Some(lease), Some(form)) = (lease, form) else {
        return response::empty(StatusCode::FORBIDDEN);
    };
    let field = |name: &str| {
        form.iter()
            .find(|(key, _)| key == name)
            .map_or("", |(_, value)| value.as_str())
    };
    if !store.csrf(lease.login(), field("csrf")) || !store.sign_out(lease.login(), field("scope") == "all") {
        return response::empty(StatusCode::FORBIDDEN);
    }
    let signed_out = HeaderValue::from_static("/login?reason=signed_out");
    let mut answer = response::redirect(&Method::POST, StatusCode::SEE_OTHER, &signed_out);
    for (name, script) in [("__Host-gm_session", false), ("__Host-gm_login", false), ("__Host-gm_csrf", true)] {
        let http_only = if script { "" } else { "; HttpOnly" };
        let cleared = format!(
            "{name}=; Path=/; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0{http_only}; Secure; SameSite=Strict"
        );
        let cleared = HeaderValue::from_str(&cleared).expect("cookie names are ASCII");
        answer.headers_mut().append(header::SET_COOKIE, cleared);
    }
    answer
}

/// A URL-encoded form body of at most 4 KiB with unique, well-formed fields.
async fn form<B: http_body::Body>(request: Request<B>) -> Option<Vec<(String, String)>> {
    let media = request.headers().get(header::CONTENT_TYPE)?.to_str().ok()?;
    let media = media.split(';').next().unwrap_or_default().trim();
    if !media.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        return None;
    }
    let mut body = pin!(request.into_body());
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
