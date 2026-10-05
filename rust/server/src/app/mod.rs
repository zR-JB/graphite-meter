//! HTTP semantics independent of version: the gate, dispatch to control endpoints, engines and the browser app, and
//! finalizing.

mod control;
pub mod finalize;
mod gate;
mod page;
pub mod query;
pub(crate) mod response;
pub mod topology;

pub(crate) use gate::MAX_HEAD_BYTES;
pub use topology::Endpoint;

use crate::{
    assets::{Asset, Assets},
    auth::Auth,
    config::{Config, ListenerKind},
    engine::{Block, Uploads},
    exchange::{EXCHANGE_BOUND, Exchange},
    lane::{Lane, Work},
    limits::{Budget, Hold, Quotas, Refusal, Transport},
    peer::{ClientKeys, Peer},
    transport::{body::Body, websocket},
};
use bytes::Buf;
use gate::Gate;
use graphite_meter_legal::Notices;
use graphite_meter_proto::{lane::LaneEnding, route::Route};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use std::{future::poll_fn, net::IpAddr, pin::pin};
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

/// What a transport does for a request.
#[derive(Debug)]
pub enum Outcome {
    /// Writes the reply within the bound its body carries.
    Response(Response<Body>),
    /// Writes the `101`, then carries the WebSocket bus that the lane bounds.
    WebSocket(Response<Body>, Lane),
    /// Ends the exchange without an answer: an HTTP/1 connection closes, an HTTP/2 or HTTP/3 stream resets.
    Abort,
}

/// Why a metered request was not admitted.
pub(super) enum Unadmitted {
    /// A trusted proxy named no single client.
    Ambiguous,
    Busy(Refusal),
}

impl From<Unadmitted> for Response<Body> {
    fn from(unadmitted: Unadmitted) -> Self {
        match unadmitted {
            Unadmitted::Ambiguous => response::ambiguous(),
            Unadmitted::Busy(refusal) => response::busy(refusal),
        }
    }
}

impl App {
    /// Draws the download block from the budget; admitted work ends when `shutdown` is cancelled.
    pub fn new(config: Config, shutdown: CancellationToken) -> Result<Self, String> {
        if config.auth.is_some() {
            return Err("authentication is unavailable in this build".into());
        }
        let budget = Budget::new(config.max_buffer_bytes);
        let block = Block::new(&budget)?;
        let alt_svc = config.listener(ListenerKind::H3).and_then(|listener| {
            let port = listen_port(&listener.address)?;
            HeaderValue::from_str(&format!("h3=\":{port}\"")).ok()
        });
        let generation = random::<16>()?.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            quotas: Quotas::new(config.limits, &budget, None),
            uploads: Uploads::new(random()?),
            auth: Auth::Off,
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

    /// A connection's share, keyed by its socket address; `None` refuses the connection.
    pub fn connection(&self, peer: IpAddr, transport: Transport) -> Option<Hold> {
        let keys = ClientKeys::connection(peer, &self.config.trusted_proxies);
        self.quotas.connection(&keys, transport)
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
            Gate::Answer(mut answer) => {
                self.finalize(&mut answer, None, false, version);
                answer
            }
            Gate::Pass { route, peer } => {
                let bootstrap = connection.endpoint.bootstrap()
                    && route == Some(Route::Probe)
                    && request.method() != Method::OPTIONS;
                match self.dispatch(request, route, &peer, connection, exchange).await {
                    Outcome::Response(mut response) => {
                        self.finalize(&mut response, route, bootstrap, version);
                        response
                    }
                    Outcome::WebSocket(mut response, lane) => {
                        self.finalize(&mut response, route, bootstrap, version);
                        return Outcome::WebSocket(response, lane);
                    }
                    Outcome::Abort => return Outcome::Abort,
                }
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
    ) -> Outcome {
        let Some(route) = route else {
            let endpoint = connection.endpoint;
            if endpoint.ui() && self.auth.claims(request.uri().path()) {
                return self.auth.handle(request, endpoint, peer).await;
            }
            return Outcome::Response(match endpoint.ui() {
                true => self.page(&request),
                false => response::status(StatusCode::NOT_FOUND),
            });
        };
        if request.method() == Method::OPTIONS {
            return Outcome::Response(response::empty(StatusCode::NO_CONTENT));
        }
        let response = match route {
            Route::Upload => return self.receive(request, peer, connection, exchange).await,
            Route::Ping => return self.websocket(&request, peer, connection, exchange),
            Route::Download => self.download(&request, peer, connection, exchange),
            Route::UploadProgress => self.progress(&request, peer, connection, exchange),
            Route::Probe => self.probe(peer, connection.endpoint),
            Route::Preflight => response::json_of(&self.preflight(&request)),
            Route::Servers => self.catalog(&request),
            Route::UploadSession => self.upload_session(),
            Route::UploadCheckpoint => self.checkpoint(&request, peer),
            Route::WtSession | Route::WsSession => self.ticket(),
            // No transport serves these yet.
            Route::WtDownload | Route::WtUpload | Route::WtPing => response::status(StatusCode::NOT_IMPLEMENTED),
        };
        Outcome::Response(response)
    }

    /// Admits a request as a lane holding a measurement handler for the operation lifetime.
    pub(super) fn admit(&self, peer: &Peer, connection: &Connection, exchange: Exchange) -> Result<Lane, Unadmitted> {
        let keys = peer.keys().ok_or(Unadmitted::Ambiguous)?;
        let hold = self.quotas.operation(&keys).map_err(Unadmitted::Busy)?;
        let lifetime = self.config.lifetimes.operation;
        Ok(exchange.admit(keys, hold, lifetime, &connection.work, &self.shutdown, peer.auth()))
    }

    /// The WebSocket bus, admitted before its handshake is checked.
    fn websocket<B>(&self, request: &Request<B>, peer: &Peer, connection: &Connection, exchange: Exchange) -> Outcome {
        let lane = match self.admit(peer, connection, exchange) {
            Ok(lane) => lane,
            Err(unadmitted) => return Outcome::Response(unadmitted.into()),
        };
        let answer = websocket::handshake(request);
        match answer.status() == StatusCode::SWITCHING_PROTOCOLS {
            true => Outcome::WebSocket(answer, lane),
            false => Outcome::Response(answer),
        }
    }

    /// `bytes=` payload bytes; HEAD and an empty download release their handler with the reply.
    fn download<B>(
        &self,
        request: &Request<B>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Response<Body> {
        let lane = match self.admit(peer, connection, exchange) {
            Ok(lane) => lane,
            Err(unadmitted) => return unadmitted.into(),
        };
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
        response
    }

    /// Reads an upload into its aggregate until the body or the lane ends; every received byte counts.
    async fn receive<B: http_body::Body>(
        &self,
        request: Request<B>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Outcome {
        let lane = match self.admit(peer, connection, exchange) {
            Ok(lane) => lane,
            Err(unadmitted) => return Outcome::Response(unadmitted.into()),
        };
        let id = query::get(request.uri().query(), "id").unwrap_or_default();
        let mut sink = match self.uploads.begin(&id, peer.keys().as_ref(), lane.clone()) {
            Ok(sink) => sink,
            Err(refusal) => return Outcome::Response(response::upload_refusal(refusal)),
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
                Some(Err(_)) => return Outcome::Abort,
                None => break lane.finish(),
            }
            if let Some(ending) = lane.due() {
                break ending;
            }
        };
        let answer = match (ending, ending.upload_refusal()) {
            (LaneEnding::Finished, _) => response::json(format!("{{\"bytes\":{}}}", sink.bytes())),
            (_, Some(refusal)) => response::upload_refusal(refusal),
            (_, None) => return Outcome::Abort,
        };
        let deadline = Instant::now() + EXCHANGE_BOUND;
        Outcome::Response(answer.map(|body| body.until(deadline)))
    }
}

/// The port of a listen address such as `:7246` or `[::]:7249`.
pub(super) fn listen_port(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|error| format!("randomness unavailable: {error}"))?;
    Ok(bytes)
}
