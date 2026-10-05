//! The one set of response constructors.

use crate::transport::body::Body;
use http::{HeaderValue, Response, StatusCode, header};

/// Plain text no browser sniffs, as Go's `http.Error` writes it.
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

/// A 405 naming the methods the route answers.
pub fn method_not_allowed(allow: &str) -> Response<Body> {
    let mut response = status(StatusCode::METHOD_NOT_ALLOWED);
    let allow = HeaderValue::from_str(allow).expect("method names are header values");
    response.headers_mut().insert(header::ALLOW, allow);
    response
}
