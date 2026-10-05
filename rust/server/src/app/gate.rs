//! The checks before dispatch: the request head, the route this endpoint mounts and its methods, and
//! authorization.

use super::{App, Connection, finalize::close, response};
use crate::{
    auth::Decision,
    peer::{Address, Peer},
    transport::body::Body,
};
use graphite_meter_proto::route::{Kind, Route};
use http::{Method, Request, Response, StatusCode, Version, header};

/// A request head, as parsed, holds at most this many bytes.
const MAX_HEAD_BYTES: usize = 32 << 10;

/// What the gate decided for a request.
pub(super) enum Gate {
    /// Dispatch it, to the route if this endpoint mounts one at its path, as the request of `peer`.
    Pass { route: Option<Route>, peer: Peer },
    /// The gate's own answer.
    Answer(Response<Body>),
}

impl App {
    pub(super) fn gate<B: http_body::Body>(&self, request: &Request<B>, connection: &Connection) -> Gate {
        if let Some(answer) = validate(request) {
            return Gate::Answer(answer);
        }
        let (endpoint, method) = (connection.endpoint, request.method());
        // Where the app is served, a method the route does not take falls through to it.
        let route = Route::from_path(request.uri().path())
            .filter(|&route| endpoint.mounts(route) && (!endpoint.ui() || allows(route, method)));
        let address = Address::resolve(connection.peer, request.headers(), &self.config.trusted_proxies);
        let peer = Peer::new(address);
        let peer = match self.auth.authorize(request, endpoint, &peer) {
            Decision::Allow(lease) => peer.with_auth(lease),
            Decision::Refuse(answer) | Decision::Handled(answer) => return Gate::Answer(answer),
        };
        match route {
            Some(route) if !allows(route, method) => {
                let mut allow: Vec<_> = methods(route).collect();
                allow.sort_unstable();
                Gate::Answer(response::method_not_allowed(&allow.join(", ")))
            }
            _ => Gate::Pass { route, peer },
        }
    }
}

/// The answer to a request breaking the head size, Host or body rules, or to `OPTIONS *`.
fn validate<B: http_body::Body>(request: &Request<B>) -> Option<Response<Body>> {
    if head_bytes(request) > MAX_HEAD_BYTES {
        return Some(response::status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE));
    }
    let version = request.version();
    let host = if version <= Version::HTTP_11 { host_refusal(request) } else { None };
    let asterisk = request.method() == Method::OPTIONS && request.uri().path() == "*";
    if host.is_none() && asterisk && version <= Version::HTTP_2 {
        return Some(response::empty(StatusCode::OK));
    }
    let text = host.or_else(|| carries_body(request).then_some("request body not accepted"))?;
    let mut response = response::text(StatusCode::BAD_REQUEST, text);
    close(response.headers_mut(), version);
    Some(response)
}

/// The method, URI and header fields, each field with its separators.
fn head_bytes<B>(request: &Request<B>) -> usize {
    let uri = request.uri();
    let uri_bytes = uri.scheme_str().map_or(0, |scheme| scheme.len() + 3)
        + uri.authority().map_or(0, |authority| authority.as_str().len())
        + uri.path_and_query().map_or(0, |path| path.as_str().len());
    let fields = request.headers().iter();
    fields.fold(request.method().as_str().len() + uri_bytes + 14, |bytes, (name, value)| {
        bytes + name.as_str().len() + value.len() + 4
    })
}

/// An HTTP/1.1 request names exactly one valid host, except a CONNECT; HTTP/1.0 may name none.
fn host_refusal<B>(request: &Request<B>) -> Option<&'static str> {
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let required = request.version() == Version::HTTP_11 && request.method() != Method::CONNECT;
    match (hosts.next(), hosts.next()) {
        (Some(_), Some(_)) => Some("400 Bad Request"),
        (None, _) if required => Some("400 Bad Request: missing required Host header"),
        (Some(host), None) if !host.as_bytes().iter().all(|&byte| host_byte(byte)) => {
            Some("400 Bad Request: malformed Host header")
        }
        _ => None,
    }
}

/// A byte a host name, an IP literal with its zone, or a port may hold.
fn host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!$%&'()*+,-.:;=[]_~".contains(&byte)
}

/// Only POST may carry a body: a nonzero declared length, or an unknown one outside HTTP/3.
fn carries_body<B: http_body::Body>(request: &Request<B>) -> bool {
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok()?.parse::<u64>().ok());
    let unknown = declared.is_none() && request.version() != Version::HTTP_3 && !request.body().is_end_stream();
    request.method() != Method::POST && (declared.is_some_and(|length| length > 0) || unknown)
}

/// The methods a route answers: those it dispatches, HEAD with GET, and OPTIONS on plain HTTP routes.
fn methods(route: Route) -> impl Iterator<Item = &'static str> {
    let dispatched = route.methods();
    let head = dispatched.contains(&"GET").then_some("HEAD");
    let options = (route.kind() == Kind::Http).then_some("OPTIONS");
    dispatched.iter().copied().chain(head).chain(options)
}

fn allows(route: Route, method: &Method) -> bool {
    methods(route).any(|allowed| allowed == method.as_str())
}
