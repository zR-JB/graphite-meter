//! The one set of response constructors.

use crate::{limits::Refusal, transport::body::Body};
use bytes::Bytes;
use graphite_meter_proto::refusal::UploadRefusal;
use http::{HeaderValue, Method, Response, StatusCode, header};
use serde::Serialize;

/// Plain text no browser sniffs, ending in a line break.
pub fn text(status: StatusCode, text: &str) -> Response<Body> {
    let mut response = Response::new(Body::full(format!("{text}\n")));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// `status` with its own text: `404 page not found`, else the reason phrase.
pub fn status(status: StatusCode) -> Response<Body> {
    match status {
        StatusCode::NOT_FOUND => text(status, "404 page not found"),
        _ => text(status, status.canonical_reason().unwrap_or_default()),
    }
}

pub fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

/// An HTML page.
pub fn html(page: String) -> Response<Body> {
    let mut response = Response::new(Body::full(page));
    let html = HeaderValue::from_static("text/html; charset=utf-8");
    response.headers_mut().insert(header::CONTENT_TYPE, html);
    response
}

/// A JSON document no cache keeps.
pub fn json(document: impl Into<Bytes>) -> Response<Body> {
    let mut response = Response::new(Body::full(document));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub fn json_of(document: &impl Serialize) -> Response<Body> {
    json(serde_json::to_vec(document).expect("documents serialize"))
}

/// A redirect to `location`; a GET answer links it in a short HTML body.
pub fn redirect(method: &Method, status: StatusCode, location: &HeaderValue) -> Response<Body> {
    let linked = matches!(*method, Method::GET | Method::HEAD);
    let mut response = match *method == Method::GET {
        true => {
            let escaped = [("&", "&amp;"), ("<", "&lt;"), (">", "&gt;"), ("\"", "&#34;"), ("'", "&#39;")]
                .iter()
                .fold(location.to_str().unwrap_or_default().to_owned(), |text, (raw, entity)| {
                    text.replace(raw, entity)
                });
            let reason = status.canonical_reason().unwrap_or_default();
            Response::new(Body::full(format!("<a href=\"{escaped}\">{reason}</a>.\n\n")))
        }
        false => Response::new(Body::empty()),
    };
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::LOCATION, location.clone());
    if linked {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    }
    response
}

/// A 405 naming the methods the route answers.
pub fn method_not_allowed(allow: &str) -> Response<Body> {
    let mut response = status(StatusCode::METHOD_NOT_ALLOWED);
    let allow = HeaderValue::from_str(allow).expect("method names are header values");
    response.headers_mut().insert(header::ALLOW, allow);
    response
}

/// A full handler pool or client share: 503 or 429, asking for a retry after a second.
pub fn busy(refusal: Refusal) -> Response<Body> {
    let mut response = status(StatusCode::from_u16(refusal.status()).expect("a client error or server error"));
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// A metered request whose trusted proxy named no single client.
pub fn ambiguous() -> Response<Body> {
    text(
        StatusCode::BAD_REQUEST,
        "client address unknown: the trusted reverse proxy's X-Real-IP is missing or does not match its \
         X-Forwarded-For; the server log names the fault",
    )
}

/// An upload refusal's message, status and name (`api/uploadrefusals.txt`).
pub fn upload_refusal(refusal: UploadRefusal) -> Response<Body> {
    let status = StatusCode::from_u16(refusal.status()).expect("a client error or server error");
    let mut response = text(status, refusal.message());
    let headers = response.headers_mut();
    headers.insert("x-graphite-upload-refusal", HeaderValue::from_static(refusal.name()));
    match refusal {
        UploadRefusal::GlobalFull | UploadRefusal::ClientFull => {
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        UploadRefusal::Revoked => {
            headers.insert("graphite-meter-auth", HeaderValue::from_static("required"));
        }
        UploadRefusal::Invalid | UploadRefusal::OwnerMismatch | UploadRefusal::Idle => {}
    }
    response
}
