//! Serves the browser app's documents, under the page's content security policy, on the listeners that carry it.

use super::{App, Endpoint, finalize::harden, response};
use crate::{peer::Peer, transport::body::Body};
use graphite_meter_proto::{
    discovery::Preflight,
    origin::{BaseUrl, Host, Origin},
};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
};

impl App {
    /// Answers a request that no route claims on a listener serving the app.
    pub(super) fn page<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Response<Body> {
        let head = request.method() == Method::HEAD;
        let mut response = match self.assets.get(request.uri().path(), request.headers()) {
            _ if request.method() != Method::GET && !head => {
                let mut response = response::text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
                let allow = HeaderValue::from_static("GET, HEAD");
                response.headers_mut().insert(header::ALLOW, allow);
                response
            }
            Some(served) => {
                let mut response = Response::new(Body::full(served.bytes));
                *response.status_mut() = served.status;
                *response.headers_mut() = served.headers;
                response
            }
            None => response::status(StatusCode::NOT_FOUND),
        };
        let length = http_body::Body::size_hint(response.body()).exact().unwrap_or_default();
        let status = response.status();
        let headers = response.headers_mut();
        if status != StatusCode::NOT_MODIFIED {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        }
        // Every response carries the policy, because a worker runs under its own script's, not the page's.
        let policy = self
            .assets
            .policy(&self.connect_sources(&self.preflight(request), secure(request, endpoint, peer)));
        let policy = HeaderValue::from_str(&policy).expect("policies hold ASCII origins");
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        harden(headers, false);
        if head {
            *response.body_mut() = Body::empty();
        }
        response
    }

    /// Lists what the page may connect to beyond itself, the catalog servers and this host's targets, less IPv6
    /// literals and, on an HTTPS page, the clear-text sources it cannot use.
    fn connect_sources(&self, preflight: &Preflight, secure: bool) -> Vec<String> {
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
        let mut seen = HashSet::new();
        sources.retain(|source| !source.contains("://[") && !(secure && mixed(source)) && seen.insert(source.clone()));
        sources
    }
}

/// Tells whether the browser loaded the page over HTTPS, on the TLS listener or through a trusted proxy that says so.
fn secure<B>(request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> bool {
    endpoint == Endpoint::H1Tls || peer.forwarded_tls(request.headers())
}

/// Spots a clear-text source, which browsers block from an HTTPS page as mixed content unless its host is loopback.
fn mixed(source: &str) -> bool {
    let (scheme, rest) = source.split_once("://").unwrap_or_default();
    let host = rest.split(':').next().unwrap_or_default().to_ascii_lowercase();
    let loopback = host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback());
    matches!(scheme, "http" | "ws") && !loopback
}

/// Pairs an origin with its WebSocket form.
fn with_socket(origin: &Origin) -> [String; 2] {
    let origin = origin.to_string();
    let socket = origin.replacen("http", "ws", 1);
    [origin, socket]
}

#[cfg(test)]
mod tests {
    #[test]
    fn clear_text_sources_off_loopback_are_mixed_content() {
        for source in ["http://meter.example:*", "ws://meter.example:7246", "http://localhost.example"] {
            assert!(super::mixed(source), "{source}");
        }
        for source in [
            "https://meter.example:*",
            "wss://meter.example:*",
            "http://127.0.0.1:7246",
            "ws://Localhost:7246",
            "http://app.localhost",
        ] {
            assert!(!super::mixed(source), "{source}");
        }
    }
}
