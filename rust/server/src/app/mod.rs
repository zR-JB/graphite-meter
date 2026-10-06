//! Version-independent HTTP: the gate, dispatch to control endpoints, engines and the browser app, finalizing.

mod control;
pub mod finalize;
mod gate;
mod page;
pub mod query;
pub(crate) mod response;

pub(crate) use gate::MAX_HEAD_BYTES;

use crate::{
    assets::{Asset, Assets},
    auth::Auth,
    config::{Config, ListenerKind},
    engine::{Block, Meter, Uploads},
    exchange::{EXCHANGE_BOUND, Exchange},
    lane::{Lane, Work},
    limits::{Budget, Hold, Pressure, Quotas, Transport},
    peer::{ClientKeys, Peer},
    transport::{
        body::Body,
        websocket,
        webtransport::{Plan, Upload},
    },
};
use bytes::Buf;
use graphite_meter_legal::Notices;
use graphite_meter_proto::{
    lane::LaneEnding,
    route::{Kind, Route},
};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use std::{future::poll_fn, net::IpAddr, pin::pin, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// The state every request shares; transports hold it in an `Arc`.
pub struct App {
    config: Config,
    budget: Budget,
    quotas: Quotas,
    uploads: Uploads,
    auth: Auth,
    assets: Assets,
    block: Block,
    /// The process's discovery generation: 32 hex digits.
    generation: String,
    /// What a bootstrap probe answer names: the HTTP/3 port.
    alt_svc: Option<HeaderValue>,
    shutdown: CancellationToken,
}

/// What the accepting transport knows of a request's connection, never anything the request claims.
#[derive(Debug, Clone)]
pub struct Connection {
    pub endpoint: Endpoint,
    /// The socket peer's address.
    pub peer: IpAddr,
    /// The admitted work on the connection.
    pub work: Work,
}

impl Connection {
    pub fn new(endpoint: Endpoint, peer: IpAddr) -> Self {
        Self { endpoint, peer, work: Work::default() }
    }
}

/// A request's outcome, or the answer refusing it.
type Admitted = Result<Outcome, Response<Body>>;

/// What a transport does for a request.
pub enum Outcome {
    /// Writes the reply within the bound its body carries.
    Response(Response<Body>),
    /// Writes the `101`, then carries the WebSocket bus that the lane bounds.
    WebSocket(Response<Body>, Lane),
    /// Accepts the WebTransport session with the head's fields, then serves the plan over the lane.
    WebTransport(Response<Body>, Lane, Plan),
    /// Ends the exchange without an answer: an HTTP/1 connection closes, an HTTP/2 or HTTP/3 stream resets.
    Abort,
}

impl App {
    /// Draws the download block from the budget; admitted work ends when `shutdown` is cancelled.
    pub fn new(config: Config, shutdown: CancellationToken) -> Result<Self, String> {
        let auth = Auth::new(config.auth.as_ref(), config.verbose)?;
        let budget = Budget::new(config.max_buffer_bytes);
        let block = Block::new(&budget, Meter::new(config.verbose))?;
        let alt_svc = config.listener(ListenerKind::H3).and_then(|listener| {
            let port = listen_port(&listener.address)?;
            HeaderValue::from_str(&format!("h3=\":{port}\"")).ok()
        });
        let password = config.auth.as_ref().and_then(|auth| auth.methods.password());
        let operator = password.map(|_| crate::peer::ClientKey::Principal(crate::auth::OPERATOR.into()));
        let generation = crate::random::<16>().iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            quotas: Quotas::new(config.limits, &budget, operator),
            uploads: Uploads::new(crate::random(), Meter::new(config.verbose)),
            auth,
            assets: Assets::embedded(config.auth.is_some(), config.result_history_default),
            budget,
            block,
            generation,
            alt_svc,
            shutdown,
            config,
        })
    }

    /// Serves `files` and `notices` as the browser app instead of the embedded ones.
    pub fn with_assets(self, files: &'static [Asset], notices: &'static Notices) -> Self {
        let assets = Assets::new(files, notices, self.config.auth.is_some(), self.config.result_history_default);
        Self { assets, ..self }
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub fn quotas(&self) -> &Quotas {
        &self.quotas
    }

    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    /// The verbose throughput lines for the `window` since the last, for each direction that moved or runs.
    pub fn transfer_lines(&self, window: Duration) -> impl Iterator<Item = (&'static str, String)> + '_ {
        [("download", self.block.meter()), ("upload", self.uploads.meter())]
            .into_iter()
            .filter_map(move |(direction, meter)| Some((direction, meter.line(window)?)))
    }

    /// A connection's share, keyed by its socket address; `None` refuses the connection.
    pub fn connection(&self, peer: IpAddr, transport: Transport) -> Option<Hold> {
        let keys = ClientKeys::connection(peer, &self.config.trusted_proxies);
        self.quotas.connection(&keys, transport)
    }

    /// Whether a QUIC handshake from `peer` needs Retry: a quarter of connections or budget used, or its source held.
    pub fn quic_retry(&self, peer: IpAddr) -> bool {
        let keys = ClientKeys::connection(peer, &self.config.trusted_proxies);
        self.quotas.connections_crowded() || self.budget.pressure() >= Pressure::Retry || self.quotas.holds_quic(&keys)
    }

    /// Receive-window credit for a connection `keys` fund; `None` past their share or the clients' half.
    pub fn window_credit(&self, keys: &ClientKeys, bytes: usize) -> Option<Hold> {
        self.quotas.credit(keys, bytes)
    }

    /// Answers a request that `exchange` bounds until it is admitted.
    pub async fn handle<B: http_body::Body>(
        &self,
        request: Request<B>,
        connection: &Connection,
        exchange: Exchange,
    ) -> Outcome {
        let (version, deadline) = (request.version(), exchange.deadline());
        let mut response = match self.gate(&request, connection) {
            Err(answer) => answer,
            Ok((route, peer)) => {
                let bootstrap = connection.endpoint.bootstrap()
                    && route == Some(Route::Probe)
                    && request.method() != Method::OPTIONS;
                let access = self.auth.access(route, peer.auth(), request.headers());
                let access = access.as_ref();
                let outcome = self.dispatch(request, route, &peer, connection, exchange).await;
                let mut outcome = outcome.unwrap_or_else(Outcome::Response);
                if let Outcome::Response(response)
                | Outcome::WebSocket(response, _)
                | Outcome::WebTransport(response, ..) = &mut outcome
                {
                    self.finalize(response, access, bootstrap, version);
                }
                let Outcome::Response(response) = outcome else { return outcome };
                response
            }
        };
        response.body_mut().bound_by_default(deadline);
        Outcome::Response(response)
    }

    async fn dispatch<B: http_body::Body>(
        &self,
        request: Request<B>,
        route: Option<Route>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Admitted {
        let Some(route) = route else {
            let endpoint = connection.endpoint;
            if endpoint.ui() && self.auth.claims(request.uri().path()) {
                return Ok(self.auth.handle(request, exchange.deadline(), peer).await);
            }
            return Ok(Outcome::Response(match endpoint.ui() {
                true => self.page(&request),
                false => response::status(StatusCode::NOT_FOUND),
            }));
        };
        if request.method() == Method::OPTIONS {
            return Ok(Outcome::Response(response::empty(StatusCode::NO_CONTENT)));
        }
        let response = match route {
            Route::Upload => return self.receive(request, peer, connection, exchange).await,
            Route::Ping => return self.websocket(&request, peer, connection, exchange),
            Route::WtDownload | Route::WtUpload | Route::WtPing => {
                return self.webtransport(&request, route, peer, connection, exchange);
            }
            Route::Download => self.download(&request, peer, connection, exchange)?,
            Route::UploadProgress => self.progress(&request, peer, connection, exchange)?,
            Route::Probe => self.probe(peer, connection.endpoint),
            Route::Preflight => response::json_of(&self.preflight(&request)),
            Route::Servers => self.catalog(&request),
            Route::UploadSession => self.upload_session(),
            Route::UploadCheckpoint => self.checkpoint(&request, peer),
            Route::WtSession => self.auth.ticket(&request, peer.auth(), Kind::WebTransport),
            Route::WsSession => self.auth.ticket(&request, peer.auth(), Kind::WebSocket),
        };
        Ok(Outcome::Response(response))
    }

    /// Admits a lane holding a handler for the operation lifetime, or a `session` for the session lifetime.
    pub(super) fn admit(
        &self,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
        session: bool,
    ) -> Result<Lane, Response<Body>> {
        let keys = peer.keys().ok_or_else(response::ambiguous)?;
        let lifetimes = &self.config.lifetimes;
        let (hold, lifetime) = match session {
            true => (self.quotas.session(&keys), lifetimes.session),
            false => (self.quotas.operation(&keys), lifetimes.operation),
        };
        let hold = hold.map_err(response::busy)?;
        Ok(exchange.admit(keys, hold, lifetime, &connection.work, &self.shutdown, peer.auth()))
    }

    /// A WebTransport session admitted before its upgrade: `/wt/ping` as an operation, transfer routes as sessions.
    fn webtransport<B>(
        &self,
        request: &Request<B>,
        route: Route,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Admitted {
        let lane = self.admit(peer, connection, exchange, route != Route::WtPing)?;
        let query = request.uri().query();
        let plan = match route {
            Route::WtDownload => Plan::Download {
                source: self.block.source(query::transfer_bytes(query)),
                streams: query::streams(query),
                datagrams: query::datagrams(query),
            },
            Route::WtUpload => Plan::Upload(Upload {
                uploads: self.uploads.clone(),
                id: query::get(query, "id").unwrap_or_default(),
                owner: peer.keys(),
                datagrams: query::datagrams(query),
            }),
            _ => Plan::Ping,
        };
        Ok(Outcome::WebTransport(response::empty(StatusCode::OK), lane, plan))
    }

    /// The WebSocket bus, admitted before its handshake is checked.
    fn websocket<B>(&self, request: &Request<B>, peer: &Peer, connection: &Connection, exchange: Exchange) -> Admitted {
        let lane = self.admit(peer, connection, exchange, false)?;
        let answer = websocket::handshake(request);
        Ok(match answer.status() == StatusCode::SWITCHING_PROTOCOLS {
            true => Outcome::WebSocket(answer, lane),
            false => Outcome::Response(answer),
        })
    }

    /// `bytes=` payload bytes; HEAD and an empty download release their handler with the reply.
    fn download<B>(
        &self,
        request: &Request<B>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Result<Response<Body>, Response<Body>> {
        let lane = self.admit(peer, connection, exchange, false)?;
        let bytes = query::transfer_bytes(request.uri().query());
        let body = match request.method() == Method::HEAD || bytes == 0 {
            true => Body::empty(),
            false => Body::download(self.block.source(bytes)).with_lane(lane),
        };
        let mut response = Response::new(body);
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes));
        Ok(response)
    }

    /// Reads an upload into its aggregate until the body or the lane ends; every received byte counts.
    async fn receive<B: http_body::Body>(
        &self,
        request: Request<B>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Admitted {
        let lane = self.admit(peer, connection, exchange, false)?;
        let id = query::get(request.uri().query(), "id").unwrap_or_default();
        let transfer = self.uploads.meter().open();
        let mut sink = match self.uploads.begin(&id, peer.keys().as_ref(), lane.clone(), transfer) {
            Ok(sink) => sink,
            Err(refusal) => return Err(response::upload_refusal(refusal)),
        };
        let mut body = pin!(request.into_body());
        let mut ended = pin!(lane.ended());
        let ending = loop {
            let frame = tokio::select! {
                biased;
                frame = poll_fn(|cx| body.as_mut().poll_frame(cx)) => frame,
                ending = &mut ended => break ending,
            };
            match frame {
                Some(Ok(frame)) => sink.record(frame.data_ref().map_or(0, Buf::remaining)),
                Some(Err(_)) => return Ok(Outcome::Abort),
                None => break lane.finish(),
            }
            if let Some(ending) = lane.due() {
                break ending;
            }
        };
        let answer = match (ending, ending.upload_refusal()) {
            (LaneEnding::Finished, _) => response::json(format!("{{\"bytes\":{}}}", sink.bytes())),
            (_, Some(refusal)) => response::upload_refusal(refusal),
            (_, None) => return Ok(Outcome::Abort),
        };
        let deadline = Instant::now() + EXCHANGE_BOUND;
        Ok(Outcome::Response(answer.map(|body| body.until(deadline))))
    }
}

/// The port of a listen address such as `:7246` or `[::]:7249`.
pub(super) fn listen_port(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

/// What accepted a connection: a TCP listener, HTTP/3's TCP companion or QUIC; its mounts follow `api/routes.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    H1,
    H1Tls,
    H2,
    H3Companion,
    Quic,
}

impl Endpoint {
    pub const ALL: [Self; 5] = [Self::H1, Self::H1Tls, Self::H2, Self::H3Companion, Self::Quic];

    /// Whether this endpoint serves `route`: the only mount decision.
    pub const fn mounts(self, route: Route) -> bool {
        match route {
            Route::Probe
            | Route::UploadSession
            | Route::UploadCheckpoint
            | Route::UploadProgress
            | Route::WtSession => true,
            Route::Preflight | Route::Servers | Route::WsSession | Route::Ping => self.ui(),
            Route::Download | Route::Upload => !matches!(self, Self::H3Companion),
            Route::WtDownload | Route::WtUpload | Route::WtPing => matches!(self, Self::Quic),
        }
    }

    /// It serves the browser app and the authentication pages, which answer every path no route claims.
    pub const fn ui(self) -> bool {
        matches!(self, Self::H1 | Self::H1Tls)
    }

    /// Its probe answers point at the HTTP/3 port.
    pub const fn bootstrap(self) -> bool {
        matches!(self, Self::H3Companion)
    }

    /// What its startup line says it serves.
    pub const fn role(self, auth: bool) -> &'static str {
        match self {
            Self::H1 if auth => {
                "HTTP/1.1 clear (trusted proxy only; refuses direct requests, redirects GET / to HTTPS)"
            }
            Self::H1 => "HTTP/1.1 clear (UI, discovery, probe, transfers, WebSockets)",
            Self::H1Tls => "HTTPS HTTP/1.1 (UI, discovery, probe, transfers, WebSockets)",
            Self::H2 => "HTTPS HTTP/2 (probe, transfers, progress)",
            Self::H3Companion => "HTTPS HTTP/1.1 companion (HTTP/3 bootstrap probe, upload and ticket control)",
            Self::Quic => "HTTP/3 (probe, transfers, progress, WebTransport)",
        }
    }
}
