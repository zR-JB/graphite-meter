//! The browser app on the listeners that serve it, under the page's content security policy.

use super::{App, finalize::harden, response};
use crate::transport::body::Body;
use graphite_meter_proto::{
    discovery::Preflight,
    origin::{BaseUrl, Host, Origin},
};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use std::net::IpAddr;

impl App {
    /// The answer to a request that no route claims on a listener serving the app.
    pub(super) fn page<B>(&self, request: &Request<B>) -> Response<Body> {
        let head = request.method() == Method::HEAD;
        let mut response = match self.assets.get(request.uri().path()) {
            _ if request.method() != Method::GET && !head => {
                let mut response = response::text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
                response
                    .headers_mut()
                    .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
                response
            }
            Some(served) => {
                let mut response = Response::new(Body::full(served.bytes));
                let headers = response.headers_mut();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(served.content_type));
                if let Some(cache) = served.cache {
                    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
                }
                response
            }
            None => response::status(StatusCode::NOT_FOUND),
        };
        let length = http_body::Body::size_hint(response.body()).exact().unwrap_or_default();
        let policy = self.assets.policy(&self.connect_sources(&self.preflight(request)));
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        let policy = HeaderValue::from_str(&policy).expect("policies hold ASCII origins");
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        harden(headers, false);
        if head {
            *response.body_mut() = Body::empty();
        }
        response
    }

    /// What the page may connect to beyond itself: the catalogue's servers and the targets offered on this host,
    /// less IPv6 literals, which a policy cannot express.
    fn connect_sources(&self, preflight: &Preflight) -> Vec<String> {
        let mut sources = Vec::new();
        for server in &self.config.catalog.servers {
            let BaseUrl::Origin(origin) = &server.url else { continue };
            if !matches!(origin.host, Host::Ip(IpAddr::V6(_))) {
                let host = &origin.host;
                sources.extend(["http", "https", "ws", "wss"].map(|scheme| format!("{scheme}://{host}:*")));
            }
            sources.extend(server.additional_origins.iter().flat_map(with_socket));
        }
        for target in &preflight.capabilities.throughput {
            if let BaseUrl::Origin(origin) = &target.base_url {
                sources.push(origin.to_string());
            }
        }
        for target in &preflight.capabilities.latency {
            if let BaseUrl::Origin(origin) = &target.base_url {
                sources.extend(with_socket(origin));
            }
        }
        let mut unique = Vec::new();
        for source in sources {
            if !source.contains("://[") && !unique.contains(&source) {
                unique.push(source);
            }
        }
        unique
    }
}

/// An origin and its WebSocket form.
fn with_socket(origin: &Origin) -> [String; 2] {
    let origin = origin.to_string();
    let socket = origin.replacen("http", "ws", 1);
    [origin, socket]
}
