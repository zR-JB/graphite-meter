//! Discovery, the probe, upload sessions, checkpoints and progress control.

use super::{App, Connection, Endpoint, listen_port, query, response};
use crate::{
    config::{ENGINE_VERSION, Listener, ListenerKind},
    exchange::Exchange,
    log::{Level, RateLimited},
    peer::Peer,
    transport::body::Body,
};
use graphite_meter_proto::{
    discovery::{
        Capabilities, LatencyTarget, LatencyTransport, Load, NegotiatedProtocol, Preflight, Probe, Protocol,
        ServerInfo, ThroughputTarget, ThroughputTransport,
    },
    origin::{BaseUrl, Origin},
    upload::Session,
};
use http::{HeaderValue, Method, Request, Response, StatusCode, header, uri::Authority};

/// A published catalogue holds at most this many bytes.
const MAX_CATALOG_BYTES: usize = 64 << 10;

static CATALOG_REFUSED: RateLimited = RateLimited::new(Level::Warn, "discovery", "invalid server catalogues");

impl App {
    /// How the server saw this request's client and connection, and the handler load.
    pub(super) fn probe(&self, peer: &Peer, endpoint: Endpoint) -> Response<Body> {
        let address = peer.address();
        let Some(source) = address.source() else {
            return response::ambiguous();
        };
        let (active, max) = self.quotas.load();
        let protocol = match endpoint {
            Endpoint::H2 => NegotiatedProtocol::Http2,
            Endpoint::Quic => NegotiatedProtocol::Http3,
            Endpoint::H1 | Endpoint::H1Tls | Endpoint::H3Companion => NegotiatedProtocol::Http1,
        };
        response::json_of(&Probe {
            client_ip: address.ip().to_string(),
            client_ip_version: if address.ip().is_ipv4() { 4 } else { 6 },
            client_ip_source: source,
            protocol_negotiated: protocol,
            load: Some(Load { active: active as u64, max: max as u64 }),
        })
    }

    /// Who this server is and the targets it offers, based on the host the request named.
    pub(super) fn preflight<B>(&self, request: &Request<B>) -> Preflight {
        let host = request_host(request);
        let stage = u64::try_from(self.config.lifetimes.stage.as_millis()).ok();
        let mut offered = Capabilities {
            upload_checkpoint: true,
            max_stage_ms: stage,
            throughput: vec![],
            latency: vec![],
        };
        for listener in self.config.listeners.iter().filter(|listener| listener.advertised) {
            let public = listener.public_origin.clone();
            let base = BaseUrl::Origin(public.unwrap_or_else(|| native(listener, &host)));
            fetch_stream(&mut offered, &base, listener.kind.protocol());
            match listener.kind {
                ListenerKind::H1 | ListenerKind::H1Tls => websocket(&mut offered, &base),
                ListenerKind::H2 => {}
                ListenerKind::H3 => {
                    for transport in [ThroughputTransport::WebTransport, ThroughputTransport::WebTransportDatagram] {
                        let target = ThroughputTarget { base_url: base.clone(), transport, protocol: Protocol::Http3 };
                        offered.throughput.push(target);
                    }
                    let transport = LatencyTransport::WebTransport;
                    offered.latency.push(LatencyTarget { base_url: base, transport });
                }
            }
        }
        let public = &self.config.public;
        for base in &public.both {
            fetch_stream(&mut offered, base, Protocol::Negotiated);
            websocket(&mut offered, base);
        }
        for base in &public.throughput {
            fetch_stream(&mut offered, base, Protocol::Negotiated);
        }
        for base in &public.latency {
            websocket(&mut offered, base);
        }
        let server = ServerInfo {
            name: self.config.name().into(),
            location: self.config.location().into(),
        };
        let (engine_version, generation) = (ENGINE_VERSION.into(), self.generation.clone());
        Preflight { server, engine_version, generation, capabilities: offered }
    }

    /// The catalogue, `self` approving the origins of its own targets.
    pub(super) fn catalog<B>(&self, request: &Request<B>) -> Response<Body> {
        let mut catalog = self.config.catalog.clone();
        let own = &mut catalog.servers[0].additional_origins;
        for base in self.preflight(request).base_urls() {
            if let BaseUrl::Origin(origin) = base
                && !own.contains(origin)
            {
                own.push(origin.clone());
            }
        }
        let document = serde_json::to_vec(&catalog).expect("catalogues serialize");
        let refused = match catalog.validate() {
            Err(error) => Some(error.to_string()),
            Ok(()) if document.len() > MAX_CATALOG_BYTES => Some("the catalogue exceeds 64 KiB".into()),
            Ok(()) => None,
        };
        match refused {
            None => response::json(document),
            Some(error) => {
                let host = request_host(request);
                CATALOG_REFUSED
                    .write(format_args!("server catalogue invalid for host {host:?}: {error}; clients get 500"));
                response::text(StatusCode::INTERNAL_SERVER_ERROR, "server catalogue unavailable")
            }
        }
    }

    pub(super) fn upload_session(&self) -> Response<Body> {
        response::json_of(&Session { upload_id: self.uploads.mint() })
    }

    /// An existing upload's counters, which never keep it alive.
    pub(super) fn checkpoint<B>(&self, request: &Request<B>, peer: &Peer) -> Response<Body> {
        let id = query::get(request.uri().query(), "id").unwrap_or_default();
        match self.uploads.checkpoint(&id, peer.keys().as_ref()) {
            Ok(counters) => response::json_of(&counters),
            Err(refusal) => response::upload_refusal(refusal),
        }
    }

    /// `GET` attaches the upload's progress feed and `DELETE` finalizes it; `HEAD` claims nothing.
    pub(super) fn progress<B>(
        &self,
        request: &Request<B>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Result<Response<Body>, Response<Body>> {
        if request.method() == Method::HEAD {
            return Ok(response::method_not_allowed("GET, DELETE"));
        }
        let lane = self.admit(peer, connection, exchange, false)?;
        let (id, owner) = (query::get(request.uri().query(), "id").unwrap_or_default(), peer.keys());
        let attached = match *request.method() {
            Method::DELETE => self.uploads.finish(&id, owner.as_ref()).map(|()| None),
            _ => self.uploads.subscribe(&id, owner.as_ref()).map(Some),
        };
        let feed = match attached {
            Ok(Some(feed)) => feed,
            Ok(None) => return Ok(response::empty(StatusCode::NO_CONTENT)),
            Err(refusal) => return Err(response::upload_refusal(refusal)),
        };
        let mut response = Response::new(Body::feed(feed).with_lane(lane));
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, no-transform"));
        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
        Ok(response)
    }
}

/// The host a request names in its URI or Host header, `localhost` without one.
fn request_host<B>(request: &Request<B>) -> String {
    let host = request.headers().get(header::HOST);
    let named = host.and_then(|host| host.to_str().ok()?.parse().ok());
    let authority: Option<Authority> = request.uri().authority().cloned().or(named);
    let authority = authority.filter(|authority| !authority.as_str().contains('@'));
    authority.map_or_else(|| "localhost".into(), |authority| authority.host().into())
}

/// A listener's own origin on `host` with its scheme and port; `localhost` when `host` forms none.
fn native(listener: &Listener, host: &str) -> Origin {
    let scheme = listener.kind.scheme().name();
    let port = listen_port(&listener.address)
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Origin::parse(&format!("{scheme}://{host}{port}"))
        .or_else(|_| Origin::parse(&format!("{scheme}://localhost{port}")))
        .expect("localhost forms an origin")
}

/// Offers `base` as a fetch-stream target; a base offered with two protocols becomes negotiated.
fn fetch_stream(offered: &mut Capabilities, base: &BaseUrl, protocol: Protocol) {
    let transport = ThroughputTransport::FetchStream;
    let mut targets = offered.throughput.iter_mut();
    let known = targets.find(|target| target.transport == transport && target.base_url == *base);
    match known {
        Some(target) if target.protocol != protocol => target.protocol = Protocol::Negotiated,
        Some(_) => {}
        None => offered
            .throughput
            .push(ThroughputTarget { base_url: base.clone(), transport, protocol }),
    }
}

fn websocket(offered: &mut Capabilities, base: &BaseUrl) {
    let transport = LatencyTransport::WebSocket;
    let mut targets = offered.latency.iter();
    if !targets.any(|target| target.transport == transport && target.base_url == *base) {
        let target = LatencyTarget { base_url: base.clone(), transport };
        offered.latency.push(target);
    }
}
