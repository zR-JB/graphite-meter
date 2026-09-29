//! Validated discovery and origin-scoped ephemeral credentials. Redirects never carry authority.
use crate::{Error, tls::Alpn};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::{Stream, TryStreamExt};
use graphite_meter_core::{
    approval,
    catalog::{Rejected, ServerCatalog, ServerEntry},
    discovery::{Preflight, Probe, Protocol},
    origin::{canonical_origin, split_url, target_origin},
    route::Route,
    wire::decode_json,
};
use graphite_meter_net::{Proxy, connect};
use http::{
    Method, Request, StatusCode, Version,
    header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue},
};
use http_body_util::{BodyExt, Empty, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    body::{Frame, Incoming},
    client::conn::{http1, http2},
};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use serde::de::DeserializeOwned;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
use tokio_rustls::TlsConnector;

type Result<T> = std::result::Result<T, Error>;
pub type Body = UnsyncBoxBody<Bytes, Error>;
pub type Response = http::Response<Incoming>;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_LIMIT: usize = 64 * 1024;
const IDLE_PER_ORIGIN: usize = 32;
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
// Go's client windows; hyper's defaults cap a 100 ms path at a few hundred Mbit/s.
const H2_STREAM_WINDOW: u32 = 32 << 20;
const H2_CONNECTION_WINDOW: u32 = 64 << 20;
/// An HTTP/2 connection that reads nothing for this long is pinged, at Go's TCP keep-alive
/// period, and closed if the peer does not answer within hyper's default 20 s.
const H2_KEEP_ALIVE: Duration = Duration::from_secs(30);
const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn empty() -> Body {
    Empty::new().map_err(|never| match never {}).boxed_unsync()
}
pub fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed_unsync()
}
pub fn streaming(body: impl Stream<Item = Result<Bytes>> + Send + 'static) -> Body {
    StreamBody::new(body.map_ok(Frame::data)).boxed_unsync()
}

struct Connections {
    proxy: Proxy,
    insecure: bool,
    pools: Mutex<HashMap<Key, Arc<Mutex<Pool>>>>,
    maintenance: OnceLock<tokio::task::JoinHandle<()>>,
    ids: AtomicU64,
}
type Key = (String, Protocol, Lanes);

/// Upload lanes keep connections of their own, as Go's upload transport does, so control requests
/// and download reads never queue behind unsent upload bodies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Lanes {
    #[default]
    Shared,
    Upload,
}

/// Held only to pick or return a connection, never across a dial or a request.
struct Pool {
    used: tokio::time::Instant,
    h1: Vec<Http1>,
    h2: Option<Http2>,
    /// A dial that may yield HTTP/2: requests wait for it rather than open their own. Its sender
    /// drops when the dial ends, however it ends.
    dialing: Option<watch::Receiver<()>>,
    /// A negotiated dial found HTTP/1.1, so each request dials its own connection at once.
    http1_only: bool,
}
impl Default for Pool {
    fn default() -> Self {
        Self {
            used: tokio::time::Instant::now(),
            h1: Vec::new(),
            h2: None,
            dialing: None,
            http1_only: false,
        }
    }
}

enum Next {
    Ready(Sender),
    Wait(watch::Receiver<()>),
    /// Dial outside the lock; the sender, if any, is the reservation later requests wait for.
    Dial(Option<watch::Sender<()>>),
}

impl Pool {
    /// `idle` false leaves idle HTTP/1.1 connections to others, for a replay after one failed.
    fn next(&mut self, multiplexed: bool, idle: bool) -> Next {
        self.used = tokio::time::Instant::now();
        if let Some(shared) = &self.h2 {
            if !shared.sender.is_closed() {
                return Next::Ready(Sender::H2(shared.clone()));
            }
            self.h2 = None;
        }
        self.h1.retain(|connection| !connection.sender.is_closed());
        if idle && let Some(index) = self.h1.iter().position(|connection| connection.sender.is_ready()) {
            return Next::Ready(Sender::H1(self.h1.swap_remove(index)));
        }
        if !multiplexed || self.http1_only {
            return Next::Dial(None);
        }
        if let Some(dialing) = &self.dialing
            && dialing.has_changed().is_ok()
        {
            return Next::Wait(dialing.clone());
        }
        let (reservation, dialing) = watch::channel(());
        self.dialing = Some(dialing);
        Next::Dial(Some(reservation))
    }

    /// Later requests dial anew rather than share this HTTP/2 connection; its open streams go on.
    fn evict(&mut self, id: u64) {
        if self.h2.as_ref().is_some_and(|shared| shared.id == id) {
            self.h2 = None;
        }
    }
}

impl Drop for Connections {
    fn drop(&mut self) {
        if let Some(task) = self.maintenance.get() {
            task.abort();
        }
    }
}

struct Http1 {
    sender: http1::SendRequest<Body>,
    absolute_form: bool,
    proxy_authorization: Option<HeaderValue>,
}
#[derive(Clone)]
struct Http2 {
    id: u64,
    sender: http2::SendRequest<Body>,
}
enum Sender {
    H1(Http1),
    H2(Http2),
}

/// How one try at a response's headers failed.
enum Attempt {
    /// On a connection an earlier request used, before any response: it may have died while idle.
    Reused(Error),
    Failed(Error),
}
impl From<Attempt> for Error {
    fn from(attempt: Attempt) -> Self {
        match attempt {
            Attempt::Reused(error) | Attempt::Failed(error) => error,
        }
    }
}

/// A copy of a bodyless request, which is safe to send again when its connection fails first.
fn replayable(request: &Request<Body>) -> Option<Request<Body>> {
    if !hyper::body::Body::is_end_stream(request.body()) {
        return None;
    }
    let mut copy = Request::new(empty());
    *copy.method_mut() = request.method().clone();
    *copy.uri_mut() = request.uri().clone();
    *copy.version_mut() = request.version();
    *copy.headers_mut() = request.headers().clone();
    Some(copy)
}

fn stream_reset(error: &hyper::Error) -> bool {
    std::error::Error::source(error)
        .and_then(|source| source.downcast_ref::<h2::Error>())
        .is_some_and(h2::Error::is_reset)
}

async fn until<T>(deadline: Option<tokio::time::Instant>, work: impl Future<Output = T>) -> Result<T> {
    match deadline {
        Some(deadline) => Ok(tokio::time::timeout_at(deadline, work).await?),
        None => Ok(work.await),
    }
}

impl Connections {
    /// TLS is set up when an HTTPS connection first needs it.
    fn new(insecure: bool, proxy: Proxy) -> Self {
        Self {
            proxy,
            insecure,
            pools: Mutex::new(HashMap::new()),
            maintenance: OnceLock::new(),
            ids: AtomicU64::new(0),
        }
    }

    /// A pooled connection, or a new one dialled outside the pool's lock, so no request waits
    /// behind another's dial unless that dial may bring the HTTP/2 connection it would share.
    /// The flag is true when the connection is not this request's own new dial.
    async fn sender(&self, origin: &str, protocol: Protocol, pool: &Mutex<Pool>, idle: bool) -> Result<(Sender, bool)> {
        let multiplexed =
            protocol == Protocol::Http2 || protocol == Protocol::Negotiated && origin.starts_with("https:");
        loop {
            let next = pool.lock().expect("connection pool poisoned").next(multiplexed, idle);
            match next {
                Next::Ready(sender) => return Ok((sender, true)),
                Next::Wait(mut dialing) => {
                    let _ = dialing.changed().await;
                }
                Next::Dial(reservation) => {
                    let sender = self.dial(origin, protocol).await?;
                    let mut pool = pool.lock().expect("connection pool poisoned");
                    match &sender {
                        Sender::H2(shared) => pool.h2 = Some(shared.clone()),
                        Sender::H1(_) => pool.http1_only |= reservation.is_some(),
                    }
                    return Ok((sender, false));
                }
            }
        }
    }

    async fn dial(&self, origin: &str, protocol: Protocol) -> Result<Sender> {
        let target = target_origin(origin)?.ok_or("missing origin")?;
        let alpn = match protocol {
            Protocol::Http1 => Alpn::Http1,
            Protocol::Http2 => Alpn::Http2,
            _ => Alpn::Negotiated,
        };
        let tls = match target.scheme.as_str() {
            "https" => Some(crate::tls::tcp(self.insecure, alpn).await?),
            _ => None,
        };
        let connection = connect(&self.proxy, &target, tls.as_ref()).await?;
        let h2 = !connection.absolute_form
            && match connection.alpn.as_deref() {
                Some(alpn) => alpn == b"h2",
                None => target.scheme == "http" && protocol == Protocol::Http2,
            };
        if protocol == Protocol::Http2 && !h2 {
            return Err("server or proxy does not support HTTP/2".into());
        }
        let io = TokioIo::new(connection.stream);
        if h2 {
            let (sender, driver) = http2::Builder::new(TokioExecutor::new())
                .timer(TokioTimer::new())
                .initial_stream_window_size(H2_STREAM_WINDOW)
                .initial_connection_window_size(H2_CONNECTION_WINDOW)
                .keep_alive_interval(H2_KEEP_ALIVE)
                .keep_alive_timeout(H2_KEEP_ALIVE_TIMEOUT)
                .keep_alive_while_idle(true)
                .handshake(io)
                .await?;
            tokio::spawn(driver);
            Ok(Sender::H2(Http2 {
                id: self.ids.fetch_add(1, Ordering::Relaxed),
                sender,
            }))
        } else {
            let (sender, driver) = http1::handshake(io).await?;
            tokio::spawn(driver);
            Ok(Sender::H1(Http1 {
                sender,
                absolute_form: connection.absolute_form,
                proxy_authorization: connection.proxy_authorization,
            }))
        }
    }

    fn maintain(self: &Arc<Self>) {
        self.maintenance.get_or_init(|| {
            let owner = Arc::downgrade(self);
            let start = tokio::time::Instant::now() + POOL_IDLE_TIMEOUT;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval_at(start, POOL_IDLE_TIMEOUT);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    let Some(owner) = owner.upgrade() else {
                        return;
                    };
                    owner.pools.lock().expect("connections poisoned").retain(|_, pool| {
                        Arc::strong_count(pool) > 1
                            || pool.lock().is_ok_and(|pool| pool.used.elapsed() < POOL_IDLE_TIMEOUT)
                    });
                }
            })
        });
    }

    /// The response headers by `deadline`, if any. A request without a body that fails on a
    /// connection an earlier request used is sent once more over a new connection, as Go's
    /// transport retries a dead reused connection.
    async fn send(
        self: &Arc<Self>,
        request: Request<Body>,
        protocol: Protocol,
        lanes: Lanes,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Response> {
        self.maintain();
        let (origin, _) = split_url(&request.uri().to_string())?;
        let origin = origin.key();
        let pool = self
            .pools
            .lock()
            .expect("connections poisoned")
            .entry((origin.clone(), protocol, lanes))
            .or_default()
            .clone();
        let replay = replayable(&request);
        match (
            self.attempt(request, &origin, protocol, &pool, deadline, true).await,
            replay,
        ) {
            (Err(Attempt::Reused(_)), Some(request)) => {
                Ok(self.attempt(request, &origin, protocol, &pool, deadline, false).await?)
            }
            (result, _) => Ok(result?),
        }
    }

    async fn attempt(
        &self,
        mut request: Request<Body>,
        origin: &str,
        protocol: Protocol,
        pool: &Mutex<Pool>,
        deadline: Option<tokio::time::Instant>,
        idle: bool,
    ) -> std::result::Result<Response, Attempt> {
        let acquired = tokio::time::Instant::now() + CONTROL_TIMEOUT;
        let acquired = deadline.map_or(acquired, |deadline| deadline.min(acquired));
        let (sender, reused) = tokio::time::timeout_at(acquired, self.sender(origin, protocol, pool, idle))
            .await
            .map_err(|elapsed| Attempt::Failed(elapsed.into()))?
            .map_err(Attempt::Failed)?;
        let (response, h2) = match sender {
            Sender::H2(mut shared) => {
                let response = until(deadline, async {
                    shared.sender.ready().await?;
                    shared.sender.send_request(request).await
                })
                .await;
                (response, Some(shared.id))
            }
            Sender::H1(mut connection) => {
                let authority = request
                    .uri()
                    .authority()
                    .ok_or_else(|| Attempt::Failed("missing authority".into()))?
                    .clone();
                let host = HeaderValue::from_str(authority.as_str()).map_err(|error| Attempt::Failed(error.into()))?;
                request.headers_mut().insert(HOST, host);
                if connection.absolute_form {
                    if let Some(authorization) = &connection.proxy_authorization {
                        request
                            .headers_mut()
                            .insert(http::header::PROXY_AUTHORIZATION, authorization.clone());
                    }
                } else {
                    let path = request
                        .uri()
                        .path_and_query()
                        .map_or("/", |path| path.as_str())
                        .parse()
                        .map_err(|error: http::uri::InvalidUri| Attempt::Failed(error.into()))?;
                    *request.uri_mut() = path;
                }
                *request.version_mut() = Version::HTTP_11;
                let response = until(deadline, connection.sender.send_request(request)).await;
                if matches!(response, Ok(Ok(_))) {
                    let mut pool = pool.lock().expect("connection pool poisoned");
                    if pool.h1.len() < IDLE_PER_ORIGIN {
                        pool.used = tokio::time::Instant::now();
                        pool.h1.push(connection);
                    }
                }
                (response, None)
            }
        };
        let failure = match response {
            Ok(Ok(response)) => return Ok(response),
            // A stream the peer or h2 reset ends alone; its connection goes on.
            Ok(Err(error)) if stream_reset(&error) => return Err(Attempt::Failed(error.into())),
            Ok(Err(error)) if reused && !error.is_user() => Attempt::Reused(error.into()),
            Ok(Err(error)) => Attempt::Failed(error.into()),
            // A timed-out HTTP/1.1 connection closes as it drops.
            Err(elapsed) => Attempt::Failed(elapsed),
        };
        if let Some(id) = h2 {
            pool.lock().expect("connection pool poisoned").evict(id);
        }
        Err(failure)
    }
}

#[derive(Clone)]
pub struct Http {
    connections: Arc<Connections>,
    lanes: Lanes,
    /// Skip TLS verification: every connection this client makes, and no grant is sent over one.
    pub(crate) insecure: bool,
    /// Each issuer's grant, as its Authorization header.
    grants: Arc<Mutex<HashMap<String, HeaderValue>>>,
    scope: Option<Arc<GrantScope>>,
}
struct GrantScope {
    issuer: String,
    targets: HashSet<String>,
}
pub struct Discovery {
    pub source: String,
    pub catalog: ServerCatalog,
    /// Entries the catalogue named but the client left out.
    pub rejected: Vec<Rejected>,
}
#[derive(Debug)]
pub struct AuthRequired {
    pub origin: String,
    /// Empty when the server named no login page, as for a revoked lane: checking the servers
    /// again finds it, as Go's client re-prepares.
    pub login_url: String,
}
impl fmt::Display for AuthRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("authentication required")?;
        match self.login_url.as_str() {
            "" => Ok(()),
            url => write!(f, " at {url}"),
        }
    }
}
impl std::error::Error for AuthRequired {}

pub(crate) fn authentication_required<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a AuthRequired> {
    crate::failure::causes(error).find_map(|cause| cause.downcast_ref())
}

/// Go's ErrApprovalExpired: the approval window closed while the server kept answering.
#[derive(Debug)]
pub struct ApprovalExpired;
impl fmt::Display for ApprovalExpired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("browser approval timed out")
    }
}
impl std::error::Error for ApprovalExpired {}

/// The approval window closed after the last poll failed to reach the server.
#[derive(Debug)]
struct ApprovalUnreachable(Error);
impl fmt::Display for ApprovalUnreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "server unreachable while waiting for browser approval: {}", self.0)
    }
}
impl std::error::Error for ApprovalUnreachable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

enum Approval {
    Pending,
    Granted,
    Unreachable(Error),
}

pub const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(120);

/// The verifier is deliberately private and has no Debug implementation.
pub struct PendingAuthorization {
    pub browser_url: String,
    pub code: String,
    pub deadline: tokio::time::Instant,
    source: String,
    verifier: zeroize::Zeroizing<String>,
    token_url: String,
}

impl Http {
    pub fn new(insecure: bool) -> Result<Self> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            return Err("install a rustls crypto provider before constructing Http".into());
        }
        Ok(Self {
            connections: Arc::new(Connections::new(insecure, Proxy::from_env())),
            lanes: Lanes::Shared,
            insecure,
            grants: Arc::new(Mutex::new(HashMap::new())),
            scope: None,
        })
    }
    /// This client for upload lanes: the same credentials, over connections of their own.
    pub(crate) fn for_upload_lanes(&self) -> Self {
        Self {
            lanes: Lanes::Upload,
            ..self.clone()
        }
    }
    /// The same credentials over a pool of its own. Each check and run takes one, as Go's client
    /// takes new transports for them, so none reuses a connection an earlier one left idle, which
    /// a sleep or a network change may have killed since.
    pub fn fresh(&self) -> Self {
        Self {
            connections: Arc::new(Connections::new(self.insecure, self.connections.proxy.clone())),
            ..self.clone()
        }
    }
    pub async fn dial(&self, origin: &str, tls: Option<&TlsConnector>) -> Result<graphite_meter_net::Connection> {
        let target = target_origin(origin)?.ok_or("missing origin")?;
        Ok(connect(&self.connections.proxy, &target, tls).await?)
    }
    #[cfg(test)]
    pub(crate) fn set_proxy(&mut self, proxy: Proxy) {
        Arc::get_mut(&mut self.connections).unwrap().proxy = proxy;
    }
    /// The issuer whose grant `origin` takes: a client bound to a server sends its grant to the
    /// server's enrolled targets alone, and an unbound client each origin's own.
    fn issuer(&self, origin: String) -> Option<String> {
        match &self.scope {
            Some(scope) => scope.targets.contains(&origin).then(|| scope.issuer.clone()),
            None => Some(origin),
        }
    }
    pub fn authorization(&self, target: &str) -> Option<HeaderValue> {
        let issuer = self.issuer(destination_origin(target).ok()?)?;
        self.grants
            .lock()
            .expect("client grants poisoned")
            .get(&issuer)
            .cloned()
    }
    /// Adds `target`'s grant to `headers`; an authenticated operation refuses TLS it does not verify.
    pub(crate) fn authorize(&self, target: &str, headers: &mut http::HeaderMap) -> Result<()> {
        if let Some(grant) = self.authorization(target) {
            if self.insecure {
                return Err("authenticated operation refuses insecure TLS".into());
            }
            headers.insert(AUTHORIZATION, grant);
        }
        Ok(())
    }
    pub fn builder(&self, method: Method, target: &str) -> Result<http::request::Builder> {
        destination_origin(target)?;
        let mut request = Request::builder()
            .method(method)
            .uri(target)
            .header(http::header::ACCEPT, "*/*");
        if let Some(headers) = request.headers_mut() {
            self.authorize(target, headers)?;
        }
        Ok(request)
    }
    /// The response headers by `deadline`; without one, as for a streamed request whose headers
    /// may wait on its body, the caller bounds it.
    pub async fn send(
        &self,
        request: Request<Body>,
        protocol: Protocol,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Response> {
        if protocol == Protocol::Http3 {
            return Err("HTTP/3 requires the native QUIC transport".into());
        }
        let target = request.uri().to_string();
        let response = self.connections.send(request, protocol, self.lanes, deadline).await?;
        self.check_status(&target, response.status(), response.headers())?;
        Ok(response)
    }
    /// A bodyless request whose response headers arrive within the control timeout.
    pub async fn request(&self, method: Method, target: &str, protocol: Protocol) -> Result<Response> {
        let request = self.builder(method, target)?.body(empty())?;
        let deadline = tokio::time::Instant::now() + CONTROL_TIMEOUT;
        self.send(request, protocol, Some(deadline)).await
    }
    pub fn check_status(&self, target: &str, status: http::StatusCode, headers: &http::HeaderMap) -> Result<()> {
        if status == StatusCode::FORBIDDEN
            && headers
                .get("graphite-meter-auth")
                .is_some_and(|value| value == "required")
        {
            let issuer = self
                .issuer(destination_origin(target)?)
                .ok_or("authentication refusal came from an unapproved target")?;
            // Both servers name no login page when they end a revoked lane (go/internal/endpoint/
            // upload.go:65-66); Go's client asks for sign-in all the same (auth.go:32-40).
            let login_url = match headers.get("graphite-meter-auth-url") {
                Some(raw) => validated_login(&issuer, raw.to_str()?)?,
                None => String::new(),
            };
            self.grants.lock().expect("client grants poisoned").remove(&issuer);
            return Err(Box::new(AuthRequired {
                origin: issuer,
                login_url,
            }));
        }
        if !status.is_success() {
            return Err(Box::new(crate::failure::HttpFailure {
                status: status.as_u16(),
                retry_after: headers
                    .get(http::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| {
                        value.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                            httpdate::parse_http_date(value)
                                .ok()
                                .and_then(|date| date.duration_since(std::time::SystemTime::now()).ok())
                        })
                    })
                    .unwrap_or_default(),
                refusal: headers
                    .get("x-graphite-upload-refusal")
                    .and_then(|value| value.to_str().ok())
                    .and_then(graphite_meter_core::failure::UploadRefusal::from_name),
            }));
        }
        Ok(())
    }
    pub async fn json<T: DeserializeOwned>(&self, method: Method, target: &str, protocol: Protocol) -> Result<T> {
        let bytes = self.control(method, target, protocol).await?;
        Ok(decode_json(&bytes)?)
    }
    async fn control(&self, method: Method, target: &str, protocol: Protocol) -> Result<Vec<u8>> {
        tokio::time::timeout(CONTROL_TIMEOUT, async {
            let response = self.request(method, target, protocol).await?;
            bounded_body(response).await
        })
        .await?
    }
    pub async fn discover(&self, source: &str) -> Result<Discovery> {
        let source = canonical_origin(source)?;
        let catalog: ServerCatalog = self
            .json(Method::GET, &url(&source, Route::Servers, &[]), Protocol::Negotiated)
            .await?;
        let (catalog, rejected) = catalog.resolve(&source).received()?;
        Ok(Discovery {
            source,
            catalog,
            rejected,
        })
    }
    pub async fn preflight(&self, entry: &ServerEntry) -> Result<Preflight> {
        let origin = canonical_origin(&entry.url)?;
        let bytes = self
            .control(Method::GET, &url(&origin, Route::Preflight, &[]), Protocol::Negotiated)
            .await?;
        let mut preflight = Preflight::decode_received(&bytes)?;
        preflight.resolve_self(&origin);
        entry.validate_discovery(&preflight)?;
        Ok(preflight)
    }
    /// The protocol is this connection's own HTTP version, as Go's `response.Proto`. The probe's
    /// `protocolNegotiated` names the server's hop, which a reverse proxy may speak differently,
    /// so it stays evidence for diagnostics only.
    pub async fn probe(&self, origin: &str, protocol: Protocol) -> Result<(Protocol, Probe)> {
        let target = url(&canonical_origin(origin)?, Route::Probe, &[]);
        let (version, bytes) = tokio::time::timeout(CONTROL_TIMEOUT, async {
            let response = self.request(Method::GET, &target, protocol).await?;
            let version = response.version();
            Ok::<_, Error>((version, bounded_body(response).await?))
        })
        .await??;
        let probe = Probe::decode(&bytes)?;
        let actual = match version {
            Version::HTTP_11 => Protocol::Http1,
            Version::HTTP_2 => Protocol::Http2,
            _ => return Err("probe used an unsupported HTTP protocol".into()),
        };
        Ok((actual, probe))
    }
    /// Bind one selected server's grant to its validated HTTPS targets. The
    /// shared grant store retains issuer tokens only; overlapping target ports
    /// cannot replace another selected server's credential.
    pub fn for_server(&self, entry: &ServerEntry, preflight: &Preflight) -> Result<Self> {
        entry.validate_discovery(preflight)?;
        let issuer = canonical_origin(&entry.url)?;
        let issuer_host = target_origin(&issuer)?.ok_or("missing grant origin")?.host;
        let mut targets = HashSet::from([issuer.clone()]);
        for raw in preflight.base_urls() {
            let origin = canonical_origin(raw)?;
            let parsed = target_origin(&origin)?.ok_or("missing target origin")?;
            if parsed.scheme == "https" && parsed.host.eq_ignore_ascii_case(&issuer_host) {
                targets.insert(origin);
            }
        }
        let mut scoped = self.clone();
        scoped.scope = Some(Arc::new(GrantScope { issuer, targets }));
        Ok(scoped)
    }
    pub fn begin_authorization(&self, source: &str, auth_url: &str) -> Result<PendingAuthorization> {
        if self.insecure {
            return Err("authenticated operation refuses insecure TLS".into());
        }
        let source = canonical_origin(source)?;
        let login = validated_login(&source, auth_url)?;
        let mut entropy = [0_u8; 32];
        getrandom::fill(&mut entropy).map_err(|_| "secure randomness unavailable")?;
        let verifier = zeroize::Zeroizing::new(URL_SAFE_NO_PAD.encode(entropy));
        zeroize::Zeroize::zeroize(&mut entropy);
        let challenge = approval::challenge(&verifier);
        let origin = destination_origin(&login)?;
        Ok(PendingAuthorization {
            browser_url: format!("{origin}/auth/cli?challenge={challenge}"),
            code: approval::verification_code(&challenge).expect("own challenge"),
            deadline: tokio::time::Instant::now() + AUTHORIZATION_TIMEOUT,
            source,
            verifier,
            token_url: format!("{origin}/auth/cli/token"),
        })
    }
    /// The caller displays browser_url/code and owns cancellation of this future. As in Go, polls
    /// outlast network errors; the deadline reports the last poll's error or ApprovalExpired.
    pub async fn poll_authorization(&self, pending: PendingAuthorization) -> Result<()> {
        let second = Duration::from_secs(1);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + second, second);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut unreachable = None;
        loop {
            // A poll the deadline cuts short keeps the previous poll's outcome, as Go's does.
            match tokio::time::timeout_at(pending.deadline, self.approval(&pending)).await {
                Ok(Ok(Approval::Granted)) => return Ok(()),
                Ok(Ok(Approval::Pending)) => unreachable = None,
                Ok(Ok(Approval::Unreachable(error))) => unreachable = Some(error),
                Ok(Err(error)) => return Err(error),
                Err(_) => {}
            }
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(pending.deadline) => {
                    return Err(match unreachable {
                        Some(error) => Box::new(ApprovalUnreachable(error)),
                        None => Box::new(ApprovalExpired),
                    });
                }
                _ = ticks.tick() => {}
            }
        }
    }
    async fn approval(&self, pending: &PendingAuthorization) -> Result<Approval> {
        let body = serde_json::to_vec(&serde_json::json!({"verifier": pending.verifier.as_str()}))?;
        let request = Request::post(&pending.token_url)
            .header(CONTENT_TYPE, "application/json")
            .body(full(body))?;
        let deadline = tokio::time::Instant::now() + CONTROL_TIMEOUT;
        let response = match self
            .connections
            .send(request, Protocol::Negotiated, Lanes::Shared, Some(deadline))
            .await
        {
            Ok(response) => response,
            Err(error) => return Ok(Approval::Unreachable(error)),
        };
        let status = response.status();
        let data = bounded_body(response).await;
        if status == StatusCode::ACCEPTED {
            return Ok(Approval::Pending);
        }
        if status != StatusCode::OK {
            return Err(format!("client approval returned HTTP {}", status.as_u16()).into());
        }
        #[derive(serde::Deserialize)]
        struct Issued {
            token: String,
        }
        let issued: Issued = data
            .and_then(|data| Ok(decode_json(&data)?))
            .map_err(|error| format!("invalid client approval response: {error}"))?;
        if issued.token.is_empty() || issued.token.len() > 8192 {
            return Err("invalid client approval token".into());
        }
        let token = zeroize::Zeroizing::new(issued.token);
        let mut header = HeaderValue::from_str(&format!("Bearer {}", token.as_str()))?;
        header.set_sensitive(true);
        let mut grants = self.grants.lock().expect("client grants poisoned");
        grants.insert(pending.source.clone(), header);
        Ok(Approval::Granted)
    }
}

pub async fn bounded_body(response: Response) -> Result<Vec<u8>> {
    let mut body = response.into_body();
    if hyper::body::Body::size_hint(&body)
        .exact()
        .is_some_and(|length| length > CONTROL_LIMIT as u64)
    {
        return Err("control response exceeds 64 KiB".into());
    }
    tokio::time::timeout(CONTROL_TIMEOUT, async {
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(chunk) = frame?.into_data() {
                if chunk.len() > CONTROL_LIMIT - bytes.len() {
                    return Err("control response exceeds 64 KiB".into());
                }
                bytes.extend_from_slice(&chunk);
            }
        }
        Ok::<_, Error>(bytes)
    })
    .await?
}
/// `route` at `origin`, with `query` form-encoded.
pub(crate) fn url(origin: &str, route: Route, query: &[(&str, &str)]) -> String {
    let mut url = format!("{origin}{}", route.path());
    if !query.is_empty() {
        url.push('?');
        url.push_str(
            &form_urlencoded::Serializer::new(String::new())
                .extend_pairs(query.iter().copied())
                .finish(),
        );
    }
    url
}

fn destination_origin(raw: &str) -> Result<String> {
    Ok(canonical_origin(&split_url(raw)?.0.key())?)
}

fn validated_login(source: &str, raw: &str) -> Result<String> {
    let source = target_origin(source)?.ok_or("missing source origin")?;
    let origin = destination_origin(raw)?;
    let login = target_origin(&origin)?.ok_or("missing login origin")?;
    if source.scheme != "https"
        || login.scheme != "https"
        || !source.host.eq_ignore_ascii_case(&login.host)
        || raw != format!("{origin}/login")
    {
        return Err("server returned an invalid authentication URL".into());
    }
    Ok(raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_core::discovery::{Capabilities, Protocol, ThroughputTarget, ThroughputTransport};

    fn http(insecure: bool) -> Http {
        let _ = crate::crypto::provider().install_default();
        Http::new(insecure).unwrap()
    }

    #[tokio::test]
    async fn cleartext_proxy_requests_reuse_absolute_form_and_keep_credentials_on_proxy() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for proxied in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let target = if proxied {
                "http://meter.test/probe".to_owned()
            } else {
                format!("http://{address}/probe")
            };
            let peer = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                for _ in 0..2 {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        head.push(stream.read_u8().await.unwrap());
                    }
                    let head = String::from_utf8(head).unwrap();
                    let expected = if proxied {
                        "GET http://meter.test/probe HTTP/1.1\r\n"
                    } else {
                        "GET /probe HTTP/1.1\r\n"
                    };
                    assert!(head.starts_with(expected), "{head}");
                    assert_eq!(
                        head.contains("proxy-authorization: Basic dXNlcjpzZWNyZXQ="),
                        proxied,
                        "{head}"
                    );
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .await
                        .unwrap();
                }
            });
            let mut http = http(false);
            http.set_proxy(Proxy::new(&format!("http://user:secret@{address}"), "", ""));
            tokio::time::timeout(Duration::from_secs(5), async {
                for _ in 0..2 {
                    let response = http.request(Method::GET, &target, Protocol::Http1).await?;
                    assert_eq!(bounded_body(response).await?, b"ok");
                }
                peer.await?;
                Ok::<_, Error>(())
            })
            .await??;
        }
        Ok(())
    }

    /// Serves each request's path from `bodies`, one request per connection.
    async fn json_peer(bodies: Vec<(&'static str, String)>) -> Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let Ok(byte) = stream.read_u8().await else { break };
                    head.push(byte);
                }
                let head = String::from_utf8_lossy(&head);
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let body = bodies
                    .iter()
                    .find(|(route, _)| *route == path)
                    .map_or("{}", |(_, body)| body.as_str());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Ok(origin)
    }

    /// Catalogue origins may end in one slash, as servers.schema.json allows, and may name
    /// international hosts, which are dialled as punycode, as Go does; an entry that is still
    /// invalid is left out alone. Preflight targets may name international hosts too.
    #[tokio::test]
    async fn discovery_takes_go_catalogue_origins_and_leaves_out_only_a_broken_entry() -> Result<()> {
        let catalog = serde_json::json!({
            "defaultSelection": ["self", "slashed"],
            "servers": [
                {"id": "self", "url": ".", "name": "self"},
                {"id": "slashed", "url": "https://remote.example:8443/", "name": "slashed"},
                {"id": "international", "url": "https://BÜCHER.example", "name": "international",
                 "additionalOrigins": ["https://münchen.example/"]},
                {"id": "broken", "url": "https://two words.example", "name": "broken"}
            ]
        });
        let preflight = serde_json::json!({
            "generation": "fixture",
            "capabilities": {
                "throughput": [{"baseUrl": "https://münchen.example", "transport": "fetch-stream", "protocol": "http2"}],
                "latency": []
            }
        });
        let origin = json_peer(vec![
            ("/servers", catalog.to_string()),
            ("/preflight", preflight.to_string()),
        ])
        .await?;
        let http = http(false);
        let discovery = http.discover(&origin).await?;
        let urls: Vec<_> = discovery
            .catalog
            .servers
            .iter()
            .map(|entry| entry.url.as_str())
            .collect();
        assert_eq!(
            urls,
            [
                origin.as_str(),
                "https://remote.example:8443",
                "https://xn--bcher-kva.example"
            ]
        );
        assert_eq!(
            discovery.catalog.servers[2].additional_origins,
            ["https://xn--mnchen-3ya.example"]
        );
        assert_eq!(discovery.catalog.default_selection, ["self", "slashed"]);
        let mut entry = discovery.catalog.servers[0].clone();
        entry.additional_origins = discovery.catalog.servers[2].additional_origins.clone();
        let preflight = http.preflight(&entry).await?;
        assert_eq!(
            preflight.capabilities.throughput[0].base_url,
            "https://xn--mnchen-3ya.example"
        );
        Ok(())
    }

    /// A proxy variable the client cannot use names itself and its fault, not a lost connection.
    #[tokio::test]
    async fn an_unusable_proxy_value_is_reported_as_such() {
        for (http_proxy, https_proxy, target, text) in [
            (
                "",
                "socks4://proxy.example:1080",
                "https://meter.test/probe",
                "HTTPS_PROXY is not a usable proxy: only HTTP, HTTPS and SOCKS5 proxies are supported",
            ),
            (
                "http://[proxy.example",
                "",
                "http://meter.test/probe",
                "HTTP_PROXY is not a usable proxy: invalid proxy URL",
            ),
        ] {
            let mut http = http(false);
            http.set_proxy(Proxy::new(http_proxy, https_proxy, ""));
            let error = http
                .request(Method::GET, target, Protocol::Negotiated)
                .await
                .unwrap_err();
            assert_eq!(crate::failure::text(error.as_ref()), text);
            assert_eq!(
                crate::failure::reason(error.as_ref(), true),
                graphite_meter_core::failure::FailureReason::PreparationFailed
            );
        }
    }

    /// A dial that stalls in its proxy handshake holds up neither another request's own dial nor
    /// the return of its response.
    #[tokio::test]
    async fn a_stalled_http1_dial_leaves_other_requests_to_dial_their_own() -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        /// A SOCKS5 proxy that is also the origin.
        async fn origin(mut stream: tokio::net::TcpStream) -> Result<()> {
            stream.read_exact(&mut [0; 3]).await?;
            stream.write_all(&[5, 0]).await?;
            let mut request = [0; 5];
            stream.read_exact(&mut request).await?;
            stream.read_exact(&mut vec![0; usize::from(request[4]) + 2]).await?;
            stream.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            loop {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(stream.read_u8().await?);
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await?;
            }
        }
        /// The first dial stops after its SOCKS greeting; later ones reach the origin.
        async fn proxy(listener: tokio::net::TcpListener, stalling: tokio::sync::oneshot::Sender<()>) -> Result<()> {
            let (_first, _) = listener.accept().await?;
            let _ = stalling.send(());
            loop {
                tokio::spawn(origin(listener.accept().await?.0));
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (stalling, stalled) = tokio::sync::oneshot::channel();
        let proxy = tokio::spawn(proxy(listener, stalling));
        let mut http = http(false);
        http.set_proxy(Proxy::new(&format!("socks5://{address}"), "", ""));
        let target = "http://meter.test/probe";
        let first = tokio::spawn({
            let http = http.clone();
            async move { http.request(Method::GET, target, Protocol::Http1).await.map(drop) }
        });
        stalled.await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..2 {
                let response = http.request(Method::GET, target, Protocol::Http1).await?;
                assert_eq!(bounded_body(response).await?, b"ok");
            }
            Ok::<_, Error>(())
        })
        .await
        .map_err(|_| "a request waited behind another request's dial")??;
        assert!(!first.is_finished());
        first.abort();
        proxy.abort();
        Ok(())
    }

    const ACCEPTED: &str = "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n";
    const ISSUED: &str =
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 19\r\n\r\n{\"token\":\"fixture\"}";

    /// Answers polls in turn, repeating the last answer; "drop" closes the connection unanswered.
    async fn token_endpoint(answers: &'static [&'static str]) -> Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/auth/cli/token", listener.local_addr()?);
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let polls = polls.clone();
                tokio::spawn(async move {
                    loop {
                        let mut head = Vec::new();
                        while !head.ends_with(b"\r\n\r\n") {
                            let Ok(byte) = stream.read_u8().await else { return };
                            head.push(byte);
                        }
                        let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .map_or(0, |length| length.trim().parse().unwrap());
                        let mut body = vec![0; length];
                        let poll = polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let answer = answers[poll.min(answers.len() - 1)];
                        if stream.read_exact(&mut body).await.is_err()
                            || answer == "drop"
                            || stream.write_all(answer.as_bytes()).await.is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        Ok(url)
    }

    fn pending(token_url: String, window: Duration) -> PendingAuthorization {
        PendingAuthorization {
            browser_url: String::new(),
            code: String::new(),
            deadline: tokio::time::Instant::now() + window,
            source: "https://meter.test".into(),
            verifier: zeroize::Zeroizing::new("verifier".into()),
            token_url,
        }
    }

    #[tokio::test]
    async fn approval_polls_through_network_errors_until_the_token_arrives() -> Result<()> {
        let url = token_endpoint(&["drop", ACCEPTED, ISSUED]).await?;
        let http = http(false);
        tokio::time::timeout(
            Duration::from_secs(10),
            http.poll_authorization(pending(url, AUTHORIZATION_TIMEOUT)),
        )
        .await??;
        assert_eq!(
            http.authorization("https://meter.test/download").unwrap(),
            "Bearer fixture"
        );
        Ok(())
    }

    #[tokio::test]
    async fn approval_deadline_reports_expiry_unless_the_last_poll_was_unreachable() -> Result<()> {
        let http = http(false);
        let answered = token_endpoint(&["drop", ACCEPTED]).await?;
        let expired = http
            .poll_authorization(pending(answered, Duration::from_millis(1500)))
            .await
            .unwrap_err();
        assert!(expired.is::<ApprovalExpired>(), "{expired}");
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await?.local_addr()?;
        let unreachable = http
            .poll_authorization(pending(
                format!("http://{closed}/auth/cli/token"),
                Duration::from_millis(1500),
            ))
            .await
            .unwrap_err();
        assert!(
            unreachable
                .to_string()
                .starts_with("server unreachable while waiting for browser approval: "),
            "{unreachable}"
        );
        assert_eq!(
            crate::failure::text(unreachable.as_ref()),
            "Server could not be reached"
        );
        Ok(())
    }

    #[test]
    fn request_destinations_preserve_validated_ascii_host_identity() {
        let http = http(false);
        for origin in [
            "https://xn--bcher-kva.example",
            "https://meter.example.",
            "https://127.0.0.1",
        ] {
            let request = http
                .builder(Method::GET, &format!("{origin}/probe"))
                .unwrap()
                .body(())
                .unwrap();
            assert_eq!(
                request.uri().host(),
                Some(target_origin(origin).unwrap().unwrap().host.as_str())
            );
        }
        for origin in [
            "https://1.2.3",
            "https://0x7f.1",
            "https://127.000.0.1",
            "https://127.0.0.1.",
            "https://BÜCHER.example",
            "https://meter.example:0",
            "https://meter.example:000",
        ] {
            assert!(
                http.builder(Method::GET, &format!("{origin}/probe")).is_err(),
                "accepted {origin}"
            );
        }
    }

    #[test]
    fn approval_urls_require_verified_same_host_canonical_https() {
        for raw in [
            "https://evil.example/login",
            "http://meter.example/login",
            "https://meter.example/login?redirect=evil",
            "https://meter.example/login#fragment",
            "https://user@meter.example/login",
            "https://meter.example/a/../login",
            "https://meter.example/\\login",
        ] {
            assert!(validated_login("https://meter.example", raw).is_err(), "{raw}");
        }
        assert!(validated_login("https://meter.example:443", "https://meter.example:8443/login").is_ok());
        assert!(
            http(true)
                .begin_authorization("https://meter.example", "https://meter.example/login")
                .is_err()
        );
    }

    #[test]
    fn grants_require_explicit_validated_target_enrollment() {
        let http = http(false);
        let fixture = HeaderValue::from_static("Bearer fixture");
        http.grants
            .lock()
            .unwrap()
            .insert("https://meter.example".into(), fixture);
        assert!(http.authorization("https://meter.example/download").is_some());
        for target in [
            "https://meter.example:8443/download",
            "http://meter.example/download",
            "https://evil.example/download",
            "https://meter.example@evil.example/download",
        ] {
            assert!(http.authorization(target).is_none());
        }
        let entry = ServerEntry {
            id: "self".into(),
            url: "https://meter.example".into(),
            name: "meter".into(),
            additional_origins: vec!["https://other.example".into()],
            ..ServerEntry::default()
        };
        let preflight = Preflight::decode(br#"{"generation":"fixture","capabilities":{"throughput":[{"baseUrl":"https://meter.example:8443","transport":"fetch-stream","protocol":"http2"},{"baseUrl":"https://other.example","transport":"fetch-stream","protocol":"http1"}],"latency":[]}}"#).unwrap();
        let scoped = http.for_server(&entry, &preflight).unwrap();
        assert!(scoped.authorization("https://meter.example:8443/upload").is_some());
        assert!(scoped.authorization("https://other.example/upload").is_none());
        assert!(http.authorization("https://meter.example:8443/upload").is_none());
        let mut withdrawn = preflight.clone();
        withdrawn.capabilities.throughput.clear();
        let withdrawn = http.for_server(&entry, &withdrawn).unwrap();
        assert!(withdrawn.authorization("https://meter.example:8443/upload").is_none());
        assert!(withdrawn.authorization("https://meter.example/upload").is_some());
    }

    #[test]
    fn overlapping_selected_targets_keep_their_issuer_grants() {
        let http = http(false);
        let first = "https://meter.example:7247";
        let second = "https://meter.example:7248";
        for (issuer, token) in [(first, "Bearer first"), (second, "Bearer second")] {
            let grant = HeaderValue::from_str(token).unwrap();
            http.grants.lock().unwrap().insert(issuer.into(), grant);
        }
        let entry = |id: &str, url: &str| ServerEntry {
            id: id.into(),
            url: url.into(),
            name: id.into(),
            ..ServerEntry::default()
        };
        let preflight = |target: &str| Preflight {
            server: Default::default(),
            engine_version: String::new(),
            generation: "fixture".into(),
            capabilities: Capabilities {
                upload_checkpoint: false,
                throughput: vec![ThroughputTarget {
                    base_url: target.into(),
                    transport: ThroughputTransport::FetchStream,
                    protocol: Protocol::Http2,
                }],
                latency: Vec::new(),
            },
        };
        let first_client = http.for_server(&entry("first", first), &preflight(second)).unwrap();
        let second_client = http.for_server(&entry("second", second), &preflight(second)).unwrap();
        let target = format!("{second}/upload");
        assert_eq!(first_client.authorization(&target).unwrap(), "Bearer first");
        assert_eq!(second_client.authorization(&target).unwrap(), "Bearer second");
        let first_request = first_client.builder(Method::POST, &target).unwrap().body(()).unwrap();
        let second_request = second_client.builder(Method::POST, &target).unwrap().body(()).unwrap();
        assert_eq!(first_request.headers()[AUTHORIZATION], "Bearer first");
        assert_eq!(second_request.headers()[AUTHORIZATION], "Bearer second");
        assert_eq!(http.authorization(&target).unwrap(), "Bearer second");

        let mut headers = http::HeaderMap::new();
        headers.insert("graphite-meter-auth", HeaderValue::from_static("required"));
        headers.insert(
            "graphite-meter-auth-url",
            HeaderValue::from_static("https://meter.example:7247/login"),
        );
        let error = first_client
            .check_status(&target, StatusCode::FORBIDDEN, &headers)
            .unwrap_err();
        assert_eq!(authentication_required(error.as_ref()).unwrap().origin, first);
        assert!(first_client.authorization(&target).is_none());
        assert_eq!(second_client.authorization(&target).unwrap(), "Bearer second");
    }
}
