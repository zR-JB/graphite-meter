//! Discovery, the probe, leases, checkpoints and progress control.

use super::{App, Connection, Endpoint, authority, query, response};
use crate::{
    admission::{self, Grant},
    config::{ENGINE_VERSION, Listener, ListenerKind},
    exchange::Exchange,
    log::{Level, RateLimited},
    peer::Peer,
    transport::body::Body,
};
use graphite_meter_proto::{
    discovery::{
        Capabilities, IpFamily, LatencyTarget, LatencyTransport, LeaseLimits, Load,
        NegotiatedProtocol, Preflight, Probe, Protocol, ServerInfo, ThroughputTarget,
        ThroughputTransport,
    },
    envelope::API_VERSION,
    lease::{Lease, LeaseRequest},
    origin::{BaseUrl, Origin},
};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};

const MAX_CATALOG_BYTES: usize = 64 << 10;

static CATALOG_REFUSED: RateLimited = RateLimited::new(
    Level::Warn,
    "catalog",
    "refused catalog requests",
    module_path!(),
);

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
            engine_version: Some(ENGINE_VERSION.into()),
            api_version: Some(API_VERSION),
            client_ip: address.ip().to_string(),
            client_ip_version: IpFamily::of(address.ip()),
            client_ip_source: source,
            protocol_negotiated: protocol,
            load: Some(Load {
                active: active as u64,
                max: max as u64,
            }),
        })
    }

    /// Who this server is and the targets it offers, based on the host the request named.
    pub(super) fn preflight<B>(&self, request: &Request<B>) -> Preflight {
        let host = request_host(request);
        let mut offered = Capabilities {
            upload_checkpoint: true,
            max_stage_ms: u64::try_from(self.config.lifetimes.stage.as_millis())
                .unwrap_or(u64::MAX),
            throughput: vec![],
            latency: vec![],
        };
        for listener in self
            .config
            .listeners
            .iter()
            .filter(|listener| listener.advertised)
        {
            let public = listener.public_origin.clone();
            let base = BaseUrl::Origin(public.unwrap_or_else(|| native(listener, &host)));
            fetch_stream(&mut offered, &base, listener.kind.protocol());
            match listener.kind {
                ListenerKind::H1 | ListenerKind::H1Tls => websocket(&mut offered, &base),
                ListenerKind::H2 => {}
                ListenerKind::H3 => {
                    for transport in [
                        ThroughputTransport::WebTransport,
                        ThroughputTransport::WebTransportDatagram,
                    ] {
                        let target = ThroughputTarget {
                            base_url: base.clone(),
                            transport,
                            protocol: Protocol::Http3,
                        };
                        offered.throughput.push(target);
                    }
                    let transport = LatencyTransport::WebTransport;
                    offered.latency.push(LatencyTarget {
                        base_url: base,
                        transport,
                    });
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
        let limits = self.config.limits;
        let count =
            |limit: std::num::NonZero<usize>| u32::try_from(limit.get()).unwrap_or(u32::MAX);
        Preflight {
            server,
            engine_version,
            api_version: API_VERSION,
            generation,
            capabilities: offered,
            lease: LeaseLimits {
                max_measurements: count(limits.operations_per_client),
                max_sessions: count(limits.sessions_per_client),
            },
        }
    }

    /// `POST` takes a lease, or resizes the one it presents, within the client's share; `DELETE` ends it.
    pub(super) async fn lease<B: http_body::Body>(
        &self,
        request: Request<B>,
        peer: &Peer,
    ) -> Response<Body> {
        let held = peer.lease().cloned();
        if request.method() == Method::DELETE {
            if let Some(lease) = held {
                self.leases.end(&lease);
            }
            return response::empty(StatusCode::NO_CONTENT);
        }
        let token = admission::presented(&request);
        let origin = request.headers().get(header::ORIGIN).cloned();
        let body = crate::auth::body(request.into_body()).await;
        let Some(asked) = body.and_then(|body| LeaseRequest::decode(&body).ok()) else {
            return response::text(StatusCode::BAD_REQUEST, "invalid lease request");
        };
        let limits = self.config.limits;
        let measurements = (asked.measurements as usize).min(limits.operations_per_client.get());
        let sessions = (asked.sessions as usize)
            .min(limits.sessions_per_client.get())
            .min(measurements);
        let grant = Grant {
            measurements,
            sessions,
        };
        let leased = match (held, token) {
            (Some(lease), Some(token)) => lease.resize(&self.quotas, grant).map(|()| token),
            _ => {
                let Some(keys) = peer.keys() else {
                    return response::ambiguous();
                };
                let created = self.leases.create(
                    &self.quotas,
                    keys,
                    peer.auth(),
                    origin.as_ref(),
                    grant,
                    &self.shutdown,
                );
                created.map(|(token, _)| token)
            }
        };
        match leased {
            Ok(token) => response::json_of(&Lease {
                token,
                measurements: measurements as u32,
                sessions: sessions as u32,
                uploads: (0..asked.uploads).map(|_| self.uploads.mint()).collect(),
            }),
            Err(refusal) => response::refusal(refusal),
        }
    }

    /// The server catalog, `self` approving the origins of its own targets.
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
        let document = serde_json::to_vec(&catalog).expect("catalogs serialize");
        let refused = match catalog.validate() {
            Err(error) => Some(error.to_string()),
            Ok(()) if document.len() > MAX_CATALOG_BYTES => {
                Some(format!("it exceeds {} KiB", MAX_CATALOG_BYTES >> 10))
            }
            Ok(()) => None,
        };
        match refused {
            None => response::json(document),
            Some(error) => {
                let host = request_host(request);
                CATALOG_REFUSED.write(format_args!(
                    "server catalog for host {host:?} invalid: {error}; clients get 500 until it is fixed"
                ));
                response::text(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server catalog unavailable",
                )
            }
        }
    }

    /// An existing upload's counters, which never keep it alive.
    pub(super) fn checkpoint<B>(&self, request: &Request<B>, peer: &Peer) -> Response<Body> {
        let id = query::get(request.uri().query(), "id").unwrap_or_default();
        match self.uploads.checkpoint(&id, peer.owner().as_ref()) {
            Ok(counters) => response::json_of(&counters),
            Err(refusal) => response::refusal(refusal),
        }
    }

    /// `GET` attaches the upload's progress feed, `HEAD` answers its head without claiming it, `DELETE` finalizes it.
    pub(super) fn progress<B>(
        &self,
        request: &Request<B>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Result<Response<Body>, Response<Body>> {
        let (id, owner) = (
            query::get(request.uri().query(), "id").unwrap_or_default(),
            peer.owner(),
        );
        let body = match *request.method() {
            Method::HEAD => {
                self.uploads
                    .watchable(&id, owner.as_ref())
                    .map_err(response::refusal)?;
                Body::empty()
            }
            Method::DELETE => {
                let _lane = self.admit(peer, connection, exchange, false)?;
                self.uploads
                    .finish(&id, owner.as_ref())
                    .map_err(response::refusal)?;
                return Ok(response::empty(StatusCode::NO_CONTENT));
            }
            _ => {
                let lane = self.admit(peer, connection, exchange, false)?;
                let feed = self
                    .uploads
                    .subscribe(&id, owner.as_ref())
                    .map_err(response::refusal)?;
                Body::feed(feed).with_lane(lane)
            }
        };
        let mut response = Response::new(body);
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-ndjson"),
        );
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-transform"),
        );
        headers.insert(
            crate::headers::X_ACCEL_BUFFERING,
            HeaderValue::from_static("no"),
        );
        Ok(response)
    }
}

/// The host a request names in its URI or Host header, `localhost` without one.
fn request_host<B>(request: &Request<B>) -> String {
    authority(request).map_or_else(|| "localhost".into(), |authority| authority.host().into())
}

/// A listener's own origin on `host` with its scheme and port; `localhost` when `host` forms none.
fn native(listener: &Listener, host: &str) -> Origin {
    let scheme = listener.kind.scheme().name();
    let port = format!(":{}", listener.address.port);
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
        None => offered.throughput.push(ThroughputTarget {
            base_url: base.clone(),
            transport,
            protocol,
        }),
    }
}

fn websocket(offered: &mut Capabilities, base: &BaseUrl) {
    let transport = LatencyTransport::WebSocket;
    let mut targets = offered.latency.iter();
    if !targets.any(|target| target.transport == transport && target.base_url == *base) {
        let target = LatencyTarget {
            base_url: base.clone(),
            transport,
        };
        offered.latency.push(target);
    }
}
