//! Version-independent HTTP: the gate, dispatch to control endpoints, engines and the browser app, finalizing.

mod control;
pub mod finalize;
mod gate;
mod page;
pub mod query;
pub(crate) mod response;

pub(crate) use gate::MAX_HEAD_BYTES;

use crate::{
    admission::Leases,
    assets::{Asset, Assets},
    auth::Auth,
    config::{Config, ListenerKind},
    engine::{Block, Meter, Uploads},
    exchange::{EXCHANGE_BOUND, Exchange},
    lane::{Lane, Work},
    limits::{Budget, Hold, Pressure, Quotas},
    lock, log,
    peer::{ClientKeys, Peer},
    transport::{
        body::Body,
        websocket::{self, Socket},
        webtransport::{Form, Plan, Upload},
    },
};
use bytes::Buf;
use graphite_meter_legal::Notices;
use graphite_meter_proto::{lane::LaneEnding, refusal::Refusal, route::Route};
use http::{HeaderValue, Method, Request, Response, StatusCode, header, uri::Authority};
use std::{
    collections::HashMap,
    future::poll_fn,
    net::{IpAddr, SocketAddr},
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub struct App {
    config: Config,
    budget: Budget,
    quotas: Quotas,
    leases: Leases,
    uploads: Uploads,
    auth: Auth,
    assets: Assets,
    block: Block,
    /// The process's discovery generation: 32 hex digits.
    generation: String,
    /// What every answer on the HTTP/3 port's TCP side names, errors too, so a browser that forgot it relearns it.
    alt_svc: Option<HeaderValue>,
    /// Under `GM_VERBOSE` only.
    connection_log: Option<Arc<ConnectionLog>>,
    shutdown: CancellationToken,
}

/// The verbose connection log, which the browser suite reads: requests, and QUIC connections per client address.
#[derive(Default)]
pub struct ConnectionLog(Mutex<HashMap<IpAddr, usize>>);

impl ConnectionLog {
    fn line(&self, peer: SocketAddr, change: isize, what: &str) {
        let held = {
            let mut held = lock(&self.0);
            let count = held.entry(peer.ip()).or_default();
            *count = count.saturating_add_signed(change);
            let count = *count;
            if count == 0 {
                held.remove(&peer.ip());
            }
            count
        };
        log!(Debug, "quic", "connection from {peer} {what}: {held} held");
    }
}

/// The log's record of an accepted QUIC connection, which logs its end when dropped.
pub struct LoggedQuic(Arc<ConnectionLog>, SocketAddr);

impl Drop for LoggedQuic {
    fn drop(&mut self) {
        self.0.line(self.1, -1, "ended");
    }
}

/// What the accepting transport knows of a request's connection, never anything the request claims.
#[derive(Debug, Clone)]
pub struct Connection {
    pub endpoint: Endpoint,
    pub peer: IpAddr,
    pub work: Work,
    pub retire: CancellationToken,
}

impl Connection {
    pub fn new(endpoint: Endpoint, peer: IpAddr) -> Self {
        Self {
            endpoint,
            peer,
            work: Work::default(),
            retire: CancellationToken::new(),
        }
    }
}

type Admitted = Result<Outcome, Response<Body>>;

#[cfg(target_os = "linux")]
pub(crate) struct FastUpload {
    pub(crate) lane: Lane,
    pub(crate) sink: crate::engine::UploadSink,
    access: Option<finalize::Access>,
    path: Option<String>,
}

#[cfg(target_os = "linux")]
pub(crate) struct FastDownload {
    pub(crate) lane: Lane,
    pub(crate) bytes: u64,
    pub(crate) sendfile: crate::engine::download::Sendfile,
    pub(crate) transfer: Option<Arc<crate::engine::Transfer>>,
    pub(crate) response: Response<Body>,
}

pub enum Outcome {
    Response(Response<Body>),
    /// Writes the `101`, then carries the WebSocket that the lane bounds.
    WebSocket(Response<Body>, Socket),
    /// Accepts the WebTransport session with the head's fields, then serves the plan over the lane.
    WebTransport(Response<Body>, Lane, Plan),
    /// Ends the exchange without an answer: an HTTP/1 connection closes, an HTTP/2 or HTTP/3 stream resets.
    Abort,
}

impl App {
    /// Draws the download block from the budget; admitted work ends when `shutdown` is canceled.
    pub fn new(config: Config, shutdown: CancellationToken) -> Result<Self, String> {
        let auth = Auth::new(config.auth.as_ref(), &config.outbound, config.verbose)?;
        let budget = Budget::new(config.max_buffer_bytes);
        let block = Block::new(&budget, Meter::new(config.verbose))?;
        let alt_svc = config.listener(ListenerKind::H3).map(|listener| {
            let public = listener.public_origin.as_ref();
            let port = public.map_or(listener.address.port, |origin| origin.port);
            HeaderValue::from_str(&format!("h3=\":{port}\"")).expect("a port is a header value")
        });
        let password = config
            .auth
            .as_ref()
            .and_then(|auth| auth.methods.password.as_ref());
        let operator =
            password.map(|_| crate::peer::ClientKey::Principal(crate::auth::Principal::operator()));
        let generation = crate::random::<16>()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            quotas: Quotas::new(config.limits, &budget, operator),
            leases: Leases::default(),
            uploads: Uploads::new(crate::random(), Meter::new(config.verbose)),
            auth,
            assets: Assets::embedded(config.auth.is_some(), config.result_history_default),
            budget,
            block,
            generation,
            alt_svc,
            connection_log: config.verbose.then(Arc::default),
            shutdown,
            config,
        })
    }

    pub fn with_assets(self, files: &'static [Asset], notices: &'static Notices) -> Self {
        let assets = Assets::new(
            files,
            notices,
            self.config.auth.is_some(),
            self.config.result_history_default,
        );
        Self { assets, ..self }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub fn shutdown(&self) -> &CancellationToken {
        &self.shutdown
    }

    pub fn quotas(&self) -> &Quotas {
        &self.quotas
    }

    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn download_sendfile(&self) -> Option<crate::engine::download::Sendfile> {
        self.block.sendfile()
    }

    /// Runs the full gate, auth, admission, and upload-aggregate join for a plain HTTP/1.1 zero-copy upload.
    #[cfg(target_os = "linux")]
    pub(crate) fn admit_fast_upload(
        &self,
        request: &Request<Body>,
        connection: &Connection,
        exchange: Exchange,
    ) -> Result<FastUpload, Response<Body>> {
        let deadline = exchange.deadline();
        let (route, peer) = match self.gate(request, connection) {
            Ok(passed) => passed,
            Err(mut answer) => {
                answer.body_mut().bound_by_default(deadline);
                return Err(answer);
            }
        };
        let access = self.auth.access(route, peer.auth(), request.headers());
        let fail = |mut resp: Response<Body>, access: Option<&finalize::Access>| {
            self.finalize(&mut resp, access);
            resp.body_mut().bound_by_default(deadline);
            resp
        };
        if route != Some(Route::Upload) || connection.endpoint.bootstrap() {
            let resp = if connection.endpoint.bootstrap() {
                response::refusal(Refusal::WrongProtocol)
            } else {
                response::status(StatusCode::NOT_FOUND)
            };
            return Err(fail(resp, access.as_ref()));
        }
        let lane = match self.admit(&peer, connection, exchange, false) {
            Ok(lane) => lane,
            Err(resp) => return Err(fail(resp, access.as_ref())),
        };
        let id = query::get(request.uri().query(), "id").unwrap_or_default();
        let transfer = self.uploads.meter().open();
        let sink = match self
            .uploads
            .begin(&id, peer.owner().as_ref(), lane.clone(), transfer)
        {
            Ok(sink) => sink,
            Err(refusal) => return Err(fail(response::refusal(refusal), access.as_ref())),
        };
        Ok(FastUpload {
            lane,
            sink,
            access,
            path: self
                .connection_log
                .is_some()
                .then(|| request.uri().path().to_owned()),
        })
    }

    /// Builds and finalizes the response for a completed fast upload; `None` aborts the connection.
    #[cfg(target_os = "linux")]
    pub(crate) fn finish_fast_upload(
        &self,
        upload: FastUpload,
        connection: &Connection,
        ending: LaneEnding,
    ) -> Option<Response<Body>> {
        let mut answer = match (ending, ending.refusal()) {
            (LaneEnding::Finished, _) => {
                response::json(format!("{{\"bytes\":{}}}", upload.sink.bytes()))
            }
            (_, Some(refusal)) => response::refusal(refusal),
            (_, None) => {
                if let Some(path) = upload.path {
                    let (peer, endpoint) = (connection.peer, connection.endpoint);
                    log!(
                        Debug,
                        "request",
                        "POST {path} from {peer} on {endpoint:?}: 0"
                    );
                }
                return None;
            }
        };
        self.finalize(&mut answer, upload.access.as_ref());
        if let Some(path) = upload.path {
            let (peer, endpoint, status) = (
                connection.peer,
                connection.endpoint,
                answer.status().as_u16(),
            );
            log!(
                Debug,
                "request",
                "POST {path} from {peer} on {endpoint:?}: {status}"
            );
        }
        Some(answer)
    }

    /// Runs gate, auth, admission, and finalize for a plain HTTP/1.1 fixed-length zero-copy download.
    #[cfg(target_os = "linux")]
    pub(crate) fn admit_fast_download(
        &self,
        request: &Request<Body>,
        connection: &Connection,
        exchange: Exchange,
    ) -> Result<Option<FastDownload>, Response<Body>> {
        let query = request.uri().query();
        if query::transfer_ms(query).is_some() || query::flag(query, "last") {
            return Ok(None);
        }
        let bytes = query::transfer_bytes(query);
        if bytes < (64 << 10) {
            return Ok(None);
        }
        let Some(sendfile) = self.block.sendfile() else {
            return Ok(None);
        };
        let deadline = exchange.deadline();
        let (route, peer) = match self.gate(request, connection) {
            Ok(passed) => passed,
            Err(mut answer) => {
                answer.body_mut().bound_by_default(deadline);
                return Err(answer);
            }
        };
        let access = self.auth.access(route, peer.auth(), request.headers());
        let fail = |mut resp: Response<Body>, access: Option<&finalize::Access>| {
            self.finalize(&mut resp, access);
            resp.body_mut().bound_by_default(deadline);
            resp
        };
        if route != Some(Route::Download) || connection.endpoint.bootstrap() {
            let resp = if connection.endpoint.bootstrap() {
                response::refusal(Refusal::WrongProtocol)
            } else {
                response::status(StatusCode::NOT_FOUND)
            };
            return Err(fail(resp, access.as_ref()));
        }
        let lane = match self.admit(&peer, connection, exchange, false) {
            Ok(lane) => lane,
            Err(resp) => return Err(fail(resp, access.as_ref())),
        };
        let mut response = Response::new(Body::empty());
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes));
        self.finalize(&mut response, access.as_ref());
        if self.connection_log.is_some() {
            let (peer, endpoint) = (connection.peer, connection.endpoint);
            log!(
                Debug,
                "request",
                "GET {} from {peer} on {endpoint:?}: 200",
                request.uri().path()
            );
        }
        let transfer = self.block.meter().open();
        Ok(Some(FastDownload {
            lane,
            bytes,
            sendfile,
            transfer,
            response,
        }))
    }

    /// The verbose throughput lines for the `window` since the last, for each direction that moved or runs.
    pub fn transfer_lines(
        &self,
        window: Duration,
    ) -> impl Iterator<Item = (&'static str, String)> + '_ {
        [
            ("download", self.block.meter()),
            ("upload", self.uploads.meter()),
        ]
        .into_iter()
        .filter_map(move |(direction, meter)| Some((direction, meter.line(window)?)))
    }

    /// A connection's share, keyed by its socket address; `None` refuses the connection.
    pub fn connection(&self, peer: IpAddr) -> Option<Hold> {
        let keys = ClientKeys::connection(peer, &self.config.trusted_proxies);
        self.quotas.connection(peer, &keys)
    }

    /// Under `GM_VERBOSE`, logs an accepted QUIC connection from `peer`, and its end once the guard drops.
    pub fn quic_accepted(&self, peer: SocketAddr) -> Option<LoggedQuic> {
        let log = self.connection_log.clone()?;
        log.line(peer, 1, "accepted");
        Some(LoggedQuic(log, peer))
    }

    /// A QUIC handshake from `peer`, held until it completes; `None` if it needs Retry first, as one does when
    /// unvalidated with a quarter of connections or budget used, or while another from its source is in progress.
    pub fn quic_handshake(&self, peer: IpAddr, validated: bool) -> Option<Hold> {
        let keys = ClientKeys::connection(peer, &self.config.trusted_proxies);
        let retry = self.quotas.connections_crowded()
            || self.budget.pressure() >= Pressure::Retry
            || self.quotas.handshaking(&keys);
        (validated || !retry).then(|| self.quotas.handshake(&keys))?
    }

    /// Receive-window credit for a connection `keys` fund; `None` past their share or the clients' half.
    pub fn window_credit(&self, keys: &ClientKeys, bytes: usize) -> Option<Hold> {
        self.quotas.credit(keys, bytes)
    }

    pub async fn handle<B: http_body::Body>(
        &self,
        request: Request<B>,
        connection: &Connection,
        exchange: Exchange,
    ) -> Outcome {
        if self.connection_log.is_none() {
            return self.answer(request, connection, exchange).await;
        }
        let (method, path) = (request.method().clone(), request.uri().path().to_owned());
        let outcome = self.answer(request, connection, exchange).await;
        let status = match &outcome {
            Outcome::Response(response)
            | Outcome::WebSocket(response, ..)
            | Outcome::WebTransport(response, ..) => response.status().as_u16(),
            Outcome::Abort => 0,
        };
        let (peer, endpoint) = (connection.peer, connection.endpoint);
        log!(
            Debug,
            "request",
            "{method} {path} from {peer} on {endpoint:?}: {status}"
        );
        outcome
    }

    async fn answer<B: http_body::Body>(
        &self,
        request: Request<B>,
        connection: &Connection,
        exchange: Exchange,
    ) -> Outcome {
        let (version, deadline) = (request.version(), exchange.deadline());
        let mut response = match self.gate(&request, connection) {
            Err(answer) => answer,
            Ok((route, peer)) => {
                let access = self.auth.access(route, peer.auth(), request.headers());
                let access = access.as_ref();
                let outcome = self
                    .dispatch(request, route, &peer, connection, exchange)
                    .await;
                let mut outcome = outcome.unwrap_or_else(Outcome::Response);
                if let Outcome::Response(response)
                | Outcome::WebSocket(response, ..)
                | Outcome::WebTransport(response, ..) = &mut outcome
                {
                    self.finalize(response, access);
                }
                let Outcome::Response(response) = outcome else {
                    return outcome;
                };
                response
            }
        };
        if connection.endpoint.bootstrap()
            && let Some(alt_svc) = &self.alt_svc
        {
            response
                .headers_mut()
                .insert(header::ALT_SVC, alt_svc.clone());
            finalize::close(response.headers_mut(), version);
        }
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
                true => self.page(&request, endpoint, peer),
                false => response::status(StatusCode::NOT_FOUND),
            }));
        };
        if request.method() == Method::OPTIONS {
            return Ok(Outcome::Response(response::empty(StatusCode::NO_CONTENT)));
        }
        let response = match route {
            Route::Download | Route::Upload if connection.endpoint.bootstrap() => {
                response::refusal(Refusal::WrongProtocol)
            }
            Route::Upload => return self.receive(request, peer, connection, exchange).await,
            Route::Ping | Route::WsProgress => {
                return self.websocket(&request, route, peer, connection, exchange);
            }
            Route::WtPing => {
                return self.webtransport(&request, WtRoute::Ping, peer, connection, exchange);
            }
            Route::WtDownload => {
                return self.webtransport(&request, WtRoute::Download, peer, connection, exchange);
            }
            Route::WtUpload => {
                return self.webtransport(&request, WtRoute::Upload, peer, connection, exchange);
            }
            Route::Download => self.download(&request, peer, connection, exchange)?,
            Route::UploadProgress => self.progress(&request, peer, connection, exchange)?,
            Route::Probe => self.probe(peer, connection.endpoint),
            Route::Lease => self.lease(request, peer).await,
            Route::Preflight => response::json_of(&self.preflight(&request)),
            Route::Servers => self.catalog(&request),
            Route::UploadCheckpoint => self.checkpoint(&request, peer),
        };
        Ok(Outcome::Response(response))
    }

    /// Admits a lane holding a handler for the operation lifetime, or a `session` for the session lifetime: against
    /// the lease it presents, else the client's share.
    pub(super) fn admit(
        &self,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
        session: bool,
    ) -> Result<Lane, Response<Body>> {
        let keys = peer.keys().ok_or_else(response::ambiguous)?;
        self.admit_as(keys, peer, connection, exchange, session)
            .map_err(response::refusal)
    }

    fn admit_as(
        &self,
        keys: ClientKeys,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
        session: bool,
    ) -> Result<Lane, Refusal> {
        let lifetimes = &self.config.lifetimes;
        let lifetime = match session {
            true => lifetimes.session,
            false => lifetimes.operation,
        };
        let (keys, admission) = match peer.lease() {
            Some(lease) => (lease.keys().clone(), lease.admit(&self.quotas, session)?),
            None if session => (keys.clone(), self.quotas.session(&keys)?.into()),
            None => (keys.clone(), self.quotas.operation(&keys)?.into()),
        };
        Ok(exchange.admit(
            keys,
            admission,
            lifetime,
            &connection.work,
            &self.shutdown,
            peer.auth(),
        ))
    }

    /// A WebTransport session admitted before its upgrade: `/wt/ping` as an operation, transfer routes as sessions.
    fn webtransport<B>(
        &self,
        request: &Request<B>,
        route: WtRoute,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Admitted {
        let lane = self.admit(peer, connection, exchange, route != WtRoute::Ping)?;
        let query = request.uri().query();
        let plan = match route {
            WtRoute::Download => Plan::Download {
                source: self.block.source(query::transfer_bytes(query)),
                form: if query::flag(query, "datagrams") {
                    Form::Datagrams
                } else {
                    Form::Streams(query::streams(query))
                },
            },
            WtRoute::Upload => Plan::Upload(Upload {
                uploads: self.uploads.clone(),
                id: query::get(query, "id").unwrap_or_default(),
                owner: peer.owner(),
                datagrams: query::flag(query, "datagrams"),
            }),
            WtRoute::Ping => Plan::Ping,
        };
        Ok(Outcome::WebTransport(
            response::empty(StatusCode::OK),
            lane,
            plan,
        ))
    }

    /// The latency bus or an upload's progress feed, admitted before its handshake is checked.
    fn websocket<B>(
        &self,
        request: &Request<B>,
        route: Route,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Admitted {
        let keys = peer.keys().ok_or_else(response::ambiguous)?;
        let lane = match self.admit_as(keys, peer, connection, exchange, false) {
            Ok(lane) => Ok(lane),
            // A feed at capacity still upgrades, so the browser reads why from its record as it would from a status.
            Err(refusal) if route == Route::WsProgress => Err(refusal),
            Err(refusal) => return Err(response::refusal(refusal)),
        };
        let accepted = match websocket::handshake(request) {
            Ok(accepted) => accepted,
            Err(refused) => return Ok(Outcome::Response(refused)),
        };
        let socket = match (route, lane) {
            (Route::Ping, Ok(lane)) => Socket::Bus(lane),
            (_, Ok(lane)) => {
                let id = query::get(request.uri().query(), "id").unwrap_or_default();
                match self.uploads.subscribe(&id, peer.owner().as_ref()) {
                    Ok(feed) => Socket::Feed(lane, feed),
                    Err(refusal) => Socket::Refused(refusal),
                }
            }
            (_, Err(refusal)) => Socket::Refused(refusal),
        };
        Ok(Outcome::WebSocket(accepted, socket))
    }

    /// `bytes=` payload bytes, ending at `ms=` on the server's clock without a length; one marked `last=` retires an
    /// HTTP/3 connection as it ends. HEAD and an empty download drop their lane at once.
    fn download<B>(
        &self,
        request: &Request<B>,
        peer: &Peer,
        connection: &Connection,
        exchange: Exchange,
    ) -> Result<Response<Body>, Response<Body>> {
        let lane = self.admit(peer, connection, exchange, false)?;
        let query = request.uri().query();
        let bytes = query::transfer_bytes(query);
        let until = query::transfer_ms(query).and_then(|ms| Instant::now().checked_add(ms));
        let body = match request.method() == Method::HEAD || bytes == 0 {
            true => Body::empty(),
            false => {
                let source = self.block.source(bytes);
                let source = match until {
                    Some(until) => source.ending_at(until),
                    None => source,
                };
                let source = match query::flag(query, "last") {
                    true => source.signalling(connection.retire.clone()),
                    false => source,
                };
                Body::download(source).with_lane(lane)
            }
        };
        let mut response = Response::new(body);
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if until.is_none() {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes));
        }
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
        let mut sink = match self
            .uploads
            .begin(&id, peer.owner().as_ref(), lane.clone(), transfer)
        {
            Ok(sink) => sink,
            Err(refusal) => return Err(response::refusal(refusal)),
        };
        let (mut body, mut ended) = (pin!(request.into_body()), pin!(lane.ended()));
        let ending = loop {
            let batch = tokio::select! {
                biased;
                batch = poll_fn(|cx| poll_batch(body.as_mut(), cx, &lane)) => batch,
                ending = &mut ended => break ending,
            };
            sink.record(batch.bytes);
            match batch.end {
                None => {}
                Some(End::Finished) => break lane.finish(),
                Some(End::Failed) => return Ok(Outcome::Abort),
                Some(End::Lane(ending)) => break ending,
            }
        };
        let answer = match (ending, ending.refusal()) {
            (LaneEnding::Finished, _) => response::json(format!("{{\"bytes\":{}}}", sink.bytes())),
            (_, Some(refusal)) => response::refusal(refusal),
            (_, None) => return Ok(Outcome::Abort),
        };
        let deadline = Instant::now() + EXCHANGE_BOUND;
        Ok(Outcome::Response(answer.map(|body| body.until(deadline))))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WtRoute {
    Ping,
    Download,
    Upload,
}

/// Upload frames taken in one read and recorded at once; an HTTP/3 frame carries one QUIC packet's data.
const FRAMES_PER_READ: usize = 32;

struct Batch {
    bytes: usize,
    end: Option<End>,
}

enum End {
    Finished,
    Failed,
    Lane(LaneEnding),
}

/// Up to `FRAMES_PER_READ` frames of `body` that are ready now, then `lane`'s check; pending only while none is.
fn poll_batch<B: http_body::Body>(
    mut body: Pin<&mut B>,
    cx: &mut Context<'_>,
    lane: &Lane,
) -> Poll<Batch> {
    let mut bytes = 0;
    for taken in 0..FRAMES_PER_READ {
        let end = match body.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                bytes += frame.data_ref().map_or(0, Buf::remaining);
                continue;
            }
            Poll::Ready(Some(Err(_))) => End::Failed,
            Poll::Ready(None) => End::Finished,
            Poll::Pending if taken == 0 => return Poll::Pending,
            Poll::Pending => break,
        };
        return Poll::Ready(Batch {
            bytes,
            end: Some(end),
        });
    }
    Poll::Ready(Batch {
        bytes,
        end: lane.due().map(End::Lane),
    })
}

/// The authority a request names in its URI or Host header, without user information.
pub(crate) fn authority<B>(request: &Request<B>) -> Option<Authority> {
    let named = || {
        request
            .headers()
            .get(header::HOST)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    };
    let authority = request.uri().authority().cloned().or_else(named);
    authority.filter(|authority| !authority.as_str().contains('@'))
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
    pub const ALL: [Self; 5] = [
        Self::H1,
        Self::H1Tls,
        Self::H2,
        Self::H3Companion,
        Self::Quic,
    ];

    /// Whether this endpoint serves `route`: the only mount decision. The companion mounts transfers to refuse them.
    pub const fn mounts(self, route: Route) -> bool {
        match route {
            Route::Probe | Route::Download | Route::Upload => true,
            Route::Lease | Route::UploadCheckpoint | Route::UploadProgress => !self.bootstrap(),
            Route::Preflight | Route::Servers | Route::Ping | Route::WsProgress => self.ui(),
            Route::WtDownload | Route::WtUpload | Route::WtPing => matches!(self, Self::Quic),
        }
    }

    /// It serves the browser app and the authentication pages, which answer every path no route claims.
    pub const fn ui(self) -> bool {
        matches!(self, Self::H1 | Self::H1Tls)
    }

    /// Its answers point at the HTTP/3 port.
    pub const fn bootstrap(self) -> bool {
        matches!(self, Self::H3Companion)
    }

    /// Its protocol, which its errors lead with.
    pub const fn protocol(self) -> &'static str {
        match self {
            Self::H1 => "HTTP/1.1",
            Self::H1Tls | Self::H3Companion => "HTTPS/1.1",
            Self::H2 => "HTTPS/2",
            Self::Quic => "HTTP/3",
        }
    }

    pub const fn roles(self, auth: bool) -> &'static str {
        const ALL: &str = "UI, discovery, probe, transfers, WebSockets";
        match self {
            Self::H1 if auth => "trusted proxy only; GET / redirects to HTTPS",
            Self::H1 | Self::H1Tls => ALL,
            Self::H2 => "probe, transfers, leases, upload control",
            Self::H3Companion => "HTTP/3 bootstrap probe",
            Self::Quic => "probe, transfers, leases, upload control, WebTransport",
        }
    }
}
