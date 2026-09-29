//! Request and response helpers the listeners and the auth controller share, as Go's net/http provides them.
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode, header};

/// The first value of the query parameter `name`, decoded, as Go's `URL.Query().Get` finds it.
pub(crate) fn query<B>(request: &Request<B>, name: &str) -> Option<String> {
    form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// Every query parameter, decoded, in order.
pub(crate) fn query_pairs<B>(request: &Request<B>) -> Vec<(String, String)> {
    form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
}

/// Go's `http.Error` text for a status: NotFound names the page, the others their reason phrase.
pub(crate) fn error_text(status: StatusCode) -> &'static str {
    match status {
        StatusCode::NOT_FOUND => "404 page not found",
        _ => status.canonical_reason().unwrap_or("error"),
    }
}

/// Go's `http.Error`: `text` and a newline, as plain text no browser sniffs.
pub(crate) fn text_body<B: From<Bytes>>(status: StatusCode, text: &str) -> Response<B> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Bytes::from(format!("{text}\n")).into())
        .expect("static error response")
}

/// Go's `http.Error` with the status's own text.
pub(crate) fn text_response<B: From<Bytes>>(status: StatusCode) -> Response<B> {
    text_body(status, error_text(status))
}

/// A 405 that names the methods the route answers.
pub(crate) fn method_not_allowed<B: From<Bytes>>(allow: &str) -> Response<B> {
    let mut response = text_response(StatusCode::METHOD_NOT_ALLOWED);
    response
        .headers_mut()
        .insert(header::ALLOW, allow.parse().expect("method names"));
    response
}

/// A JSON document no cache keeps.
pub(crate) fn json_response<B: From<Bytes>>(document: impl Into<Bytes>) -> Response<B> {
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(document.into().into())
        .expect("static JSON response")
}

/// Go's `http.Redirect` to `location`, which must be a valid header value: a GET's or HEAD's answer is HTML, and a
/// GET's also links the destination.
pub(crate) fn redirect<B: From<Bytes>>(method: &Method, status: StatusCode, location: &str) -> Response<B> {
    let mut response = Response::builder().status(status).header(header::LOCATION, location);
    if matches!(*method, Method::GET | Method::HEAD) {
        response = response.header(header::CONTENT_TYPE, "text/html; charset=utf-8");
    }
    let body = if method == Method::GET {
        redirect_link(status, location)
    } else {
        String::new()
    };
    response.body(Bytes::from(body).into()).expect("valid redirect")
}

/// The link a GET's redirect carries, escaped as Go's `htmlEscape`.
pub(crate) fn redirect_link(status: StatusCode, location: &str) -> String {
    let mut link = String::with_capacity(location.len());
    for character in location.chars() {
        match character {
            '&' => link.push_str("&amp;"),
            '<' => link.push_str("&lt;"),
            '>' => link.push_str("&gt;"),
            '"' => link.push_str("&#34;"),
            '\'' => link.push_str("&#39;"),
            character => link.push(character),
        }
    }
    format!(
        "<a href=\"{link}\">{}</a>.\n\n",
        status.canonical_reason().unwrap_or_default()
    )
}

/// Go's `cleanPath`: `path` without empty segments, keeping a trailing slash; `None` when it is clean already.
/// Callers have refused dot segments.
pub(crate) fn clean_path(path: &str) -> Option<String> {
    let segments: Vec<_> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    let mut clean = format!("/{}", segments.join("/"));
    if path.ends_with('/') && clean != "/" {
        clean.push('/');
    }
    (path.starts_with('/') && clean != path).then_some(clean)
}

pub(crate) fn empty_response<B: From<Bytes>>(status: StatusCode) -> Response<B> {
    let mut response = Response::new(Bytes::new().into());
    *response.status_mut() = status;
    response
}
