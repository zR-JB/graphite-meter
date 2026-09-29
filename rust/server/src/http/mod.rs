//! Shared endpoint state and an owned HTTP/1 connection loop.

mod body;
mod http2;
mod http3;
mod lifecycle;
mod quic;
pub(crate) mod response;
pub(crate) mod topology;
mod upload;
mod websocket;
mod webtransport;
pub(crate) use body::ResponseBody;
use body::{Operation, Operations, check_operations, holds_permit};
use lifecycle::AdmittedWork;
pub use quic::QuicEndpoint;
use topology::Accepted;

use crate::{
    ServerError,
    admission::{Admission, Permit},
    auth::{
        AuthLease, AuthRoute,
        password_login::LOCAL_OPERATOR,
        policy::{Authorization, AuthorizedRequest, Connection},
        route as auth_route,
    },
    budget::{self, DOWNLOAD_BLOCK_BYTES, H2_FLOOR_BYTES, QUIC_CREDIT_BYTES},
    client_address,
    config::{AuthMode, ConfigError, NativeKind, ValidatedConfig},
    connections::{Connections, QUIC_PER_CLIENT},
    cors::Access,
    discovery::Discovery,
    sync::lock,
    timeouts::{CONTROL, IDLE_BOUND, SHUTDOWN_GRACE},
    upload::Owner,
    upload::UploadStore,
};
use bytes::Bytes;
use graphite_meter_core::{
    route::{self, Kind, Route},
    wire::MAX_TRANSFER_BYTES,
};
use http::{Method, Request, Response, StatusCode, header};
use hyper::{
    body::{Body, Frame, SizeHint},
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use response::{empty_response, json_response, method_not_allowed, query, text_body, text_response};
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::Sleep,
};

const MAX_HEADER_BYTES: usize = 32 * 1024;
const DEFAULT_DOWNLOAD_BYTES: u64 = 25 * 1024 * 1024;
/// As Go's `http.Server`, a failed accept retries after a delay that doubles from the first bound to the last.
const ACCEPT_RETRY_FIRST: Duration = Duration::from_millis(5);
const ACCEPT_RETRY_LAST: Duration = Duration::from_secs(1);
/// Go's TCP_NOTSENT_LOWAT for HTTP/2, which keeps unsent downloads in the scheduler where control replies interleave.
#[cfg(target_os = "linux")]
const H2_NOTSENT_LOWAT_BYTES: u32 = 64 * 1024;

pub struct HttpServer {
    pub(crate) config: Arc<ValidatedConfig>,
    discovery: Discovery,
    admission: Admission,
    connections: Connections,
    stopping: tokio::sync::watch::Sender<bool>,
    memory: Arc<budget::MemoryBudget>,
    client_credit: Arc<budget::ClientCredit>,
    handshake_bytes: AtomicUsize,
    endpoint_bytes: AtomicUsize,
    download_block: Bytes,
    download_meter: crate::meter::Meter,
    peers: crate::log::PeerLog,
    _download_memory: budget::Lease,
    uploads: UploadStore,
    auth: Option<crate::auth::http::Service>,
    assets: crate::assets::Assets,
}

impl HttpServer {
    pub(crate) fn log_admission(&self) {
        let (handlers, limits, connections) = (self.admission.stats(), self.config.limits, self.connections.stats());
        crate::log!(
            "[gm:admission] handlers {} active / {} peak, rejected {} pool + {} client; sessions {} active / {} max, \
             {} per client, rejected {} budget + {} client; connections {} active / {} peak, rejected {} global + {} \
             client",
            handlers.active,
            handlers.peak,
            handlers.refused_pool,
            handlers.refused_client,
            handlers.sessions,
            limits.sessions,
            limits.sessions_per_client,
            handlers.sessions_refused_budget,
            handlers.sessions_refused_client,
            connections.active,
            connections.peak,
            connections.rejected_global,
            connections.rejected_client
        );
    }

    pub(crate) fn log_transfers(&self, window: Duration) {
        self.download_meter.log("download", window);
        self.uploads.log_transfer(window);
    }

    pub async fn initialize_auth(&self) -> Result<(), ServerError> {
        if let Some(auth) = &self.auth {
            auth.configure_logging(self.config.verbose);
            auth.initialize().await?;
        }
        Ok(())
    }

    pub(crate) async fn security_log(&self) {
        self.auth.as_ref().expect("authentication enabled").security_log().await;
    }

    pub fn cover_handshake(&self, handshake_bytes: usize) -> Result<(), ConfigError> {
        let endpoint = self.endpoint_bytes.load(Ordering::Relaxed);
        budget::check(
            &self.config,
            self.memory.limit,
            handshake_bytes,
            (endpoint != 0).then_some(endpoint),
        )?;
        self.handshake_bytes.store(handshake_bytes, Ordering::Relaxed);
        Ok(())
    }

    pub fn new(config: ValidatedConfig) -> Result<Self, ServerError> {
        let bytes = config.max_buffer_bytes;
        Self::with_memory(config, bytes)
    }

    fn with_memory(config: ValidatedConfig, bytes: usize) -> Result<Self, ServerError> {
        let config = Arc::new(config);
        let auth = if config.auth.mode == AuthMode::Off {
            None
        } else {
            Some(crate::auth::http::Service::new(
                &config.auth,
                config.trusted_proxies.clone(),
                None,
            )?)
        };
        let assets = crate::assets::Assets::new(auth.is_some(), config.result_history_default);
        let admission = Admission::new(config.limits);
        let discovery = Discovery::new(config.clone(), admission.clone())?;
        let connections = Connections::new(
            config.max_connections,
            config.max_connections_per_client,
            config.trusted_proxies.clone(),
        );
        let memory = budget::MemoryBudget::new(bytes);
        // A window on each QUIC connection a client may hold, as Go grants every connection its window.
        let quic_per_client = config
            .max_connections_per_client
            .min(config.max_connections)
            .min(QUIC_PER_CLIENT);
        let client_credit = budget::ClientCredit::new(
            quic_per_client.saturating_mul(QUIC_CREDIT_BYTES),
            config
                .auth
                .mode
                .password()
                .then(|| Owner::principal_key(LOCAL_OPERATOR)),
        );
        let download_memory = memory
            .lease(DOWNLOAD_BLOCK_BYTES)
            .ok_or("server memory budget cannot cover the download block")?;
        let mut block = vec![0; DOWNLOAD_BLOCK_BYTES];
        getrandom::fill(&mut block).map_err(|_| "download payload randomness unavailable")?;
        let download_meter = crate::meter::Meter::new(config.verbose);
        let uploads =
            UploadStore::with_meter(crate::meter::Meter::new(config.verbose)).ok_or("upload session mint failed")?;
        Ok(Self {
            config,
            discovery,
            admission,
            connections,
            stopping: tokio::sync::watch::channel(false).0,
            memory,
            client_credit,
            handshake_bytes: AtomicUsize::new(0),
            endpoint_bytes: AtomicUsize::new(0),
            download_block: block.into(),
            download_meter,
            peers: Default::default(),
            _download_memory: download_memory,
            uploads,
            auth,
            assets,
        })
    }

    /// The caller owns listener binding and shutdown. No connection task escapes this scope, including on accept
    /// errors or server cancellation; connection capacity covers the bounded TLS handshake and the whole connection.
    pub async fn serve(
        self: Arc<Self>,
        kind: NativeKind,
        listener: TcpListener,
        tls: Option<Arc<rustls::ServerConfig>>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ServerError> {
        let h2 = kind == NativeKind::H2;
        let spec = topology::tcp(kind, self.auth.is_some());
        // The HTTP/3 companion's probe advertises the QUIC port under the HTTP/3 public origin.
        let bootstrap = match self.config.listener(NativeKind::H3).public_origin.as_str() {
            _ if !spec.topology.bootstrap => None,
            "" => Some(listener.local_addr()?.port()),
            public => Some(
                graphite_meter_core::origin::target_origin(public)?
                    .ok_or("HTTP/3 public origin is missing")?
                    .port_number(),
            ),
        };
        let tls = tls.map(|tls| {
            let mut tls = (*tls).clone();
            tls.alpn_protocols = vec![spec.alpn.to_vec()];
            tokio_rustls::TlsAcceptor::from(Arc::new(tls))
        });
        tokio::pin!(shutdown);
        let mut tasks = JoinSet::new();
        let mut accept_delay = Duration::ZERO;
        let mut accept_at = tokio::time::Instant::now();
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                Some(_) = tasks.join_next() => {}
                accepted = async {
                    if !accept_delay.is_zero() {
                        tokio::time::sleep_until(accept_at).await;
                    }
                    listener.accept().await
                } => {
                    let (socket, peer) = match accepted {
                        Ok(accepted) => accepted,
                        Err(_) => {
                            accept_delay = (accept_delay * 2).clamp(ACCEPT_RETRY_FIRST, ACCEPT_RETRY_LAST);
                            accept_at = tokio::time::Instant::now() + accept_delay;
                            continue;
                        }
                    };
                    accept_delay = Duration::ZERO;
                    let Ok(permit) = self.connections.acquire(peer, false) else {
                        continue;
                    };
                    // Small control replies must not wait for Nagle buffering.
                    let _ = socket.set_nodelay(true);
                    #[cfg(target_os = "linux")]
                    if h2 {
                        let _ = socket2::SockRef::from(&socket).set_tcp_notsent_lowat(H2_NOTSENT_LOWAT_BYTES);
                    }
                    let memory = if h2 {
                        let Some(lease) = self.memory.lease(H2_FLOOR_BYTES) else { continue; };
                        Some(lease)
                    } else { None };
                    let server = self.clone();
                    let tls = tls.clone();
                    let accepted = Accepted { peer, tls: tls.is_some(), topology: spec.topology };
                    tasks.spawn(async move {
                        let _permit = permit;
                        let _memory = memory;
                        server.serve_tcp_connection(socket, accepted, tls, h2, bootstrap).await;
                    });
                }
            }
        };
        self.stopping.send_replace(true);
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, async { while tasks.join_next().await.is_some() {} }).await;
        tasks.shutdown().await;
        result
    }

    /// One accepted connection: its TLS handshake within the control bound, then HTTP/1.1, or HTTP/2 on the HTTP/2
    /// listener when the handshake chose it.
    async fn serve_tcp_connection(
        self: Arc<Self>,
        socket: TcpStream,
        accepted: Accepted,
        tls: Option<tokio_rustls::TlsAcceptor>,
        h2: bool,
        bootstrap: Option<u16>,
    ) {
        let Some(tls) = tls else {
            return self.serve_http1_connection(socket, accepted, None).await;
        };
        let peer = SocketAddr::new(accepted.peer.ip().to_canonical(), accepted.peer.port());
        let stream = tokio::select! {
            biased;
            _ = stopped(self.stopping.clone()) => return,
            result = tokio::time::timeout(CONTROL, tls.accept(socket).into_fallible()) => match result {
                Ok(Ok(stream)) => stream,
                Ok(Err((error, _socket))) => {
                    self.peers.write(format_args!("[gm:http] http: TLS handshake error from {peer}: {error}"));
                    return;
                }
                Err(_) => {
                    self.peers.write(format_args!("[gm:http] http: TLS handshake error from {peer}: timed out"));
                    return;
                }
            },
        };
        if !h2 {
            self.serve_http1_connection(stream, accepted, bootstrap).await;
        } else if stream.get_ref().1.alpn_protocol() == Some(b"h2") {
            self.serve_http2_connection(stream, accepted).await;
        }
    }

    async fn serve_http1_connection<T>(self: Arc<Self>, stream: T, accepted: Accepted, bootstrap_port: Option<u16>)
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let stopping = self.stopping.clone();
        let operations = Arc::new(Mutex::new(Vec::new()));
        let upgrade = Arc::new(Mutex::new(None));
        let pending_upgrade = upgrade.clone();
        let lifecycle = Arc::new(Mutex::new(Http1Lifecycle::Headers(Box::pin(tokio::time::sleep(
            CONTROL,
        )))));
        // Wrap the TLS stream, not its raw socket: a successful flush must also
        // drain encrypted records before releasing the response's capacity.
        let io = DeadlineIo {
            inner: http2::WriteProgressIo::new(stream, IDLE_BOUND),
            operations: operations.clone(),
            lifecycle: Some(lifecycle.clone()),
        };
        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
            let server = self.clone();
            let operations = operations.clone();
            let pending_upgrade = pending_upgrade.clone();
            let head = request.method() == Method::HEAD;
            let route = route::lookup(request.uri().path());
            let probe = route == Some(Route::Probe) && request.method() != Method::OPTIONS;
            let lifecycle = lifecycle.clone();
            *lock(&lifecycle) = Http1Lifecycle::Active {
                complete: false,
                control: Some(Box::pin(tokio::time::sleep(CONTROL))),
            };
            async move {
                let mut response =
                    if bootstrap_port.is_some() && !route.is_some_and(|route| accepted.topology.mounts(route)) {
                        text_response(StatusCode::NOT_FOUND)
                    } else {
                        server
                            .respond_incoming(request, accepted, &operations, Some(&pending_upgrade))
                            .await?
                    };
                if let Some(port) = bootstrap_port
                    && probe
                    && response.status().is_success()
                {
                    response
                        .headers_mut()
                        .insert(header::ALT_SVC, format!("h3=\":{port}\"").parse().expect("valid port"));
                    response
                        .headers_mut()
                        .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
                }
                let admitted = response.body().operation.as_ref().is_some_and(|operation| {
                    let mut operation = lock(operation);
                    operation.body_complete |= head;
                    operation.permit.is_some()
                });
                let mut state = lock(&lifecycle);
                let control = match &mut *state {
                    Http1Lifecycle::Active { control, .. } => control.take(),
                    _ => None,
                };
                *state = if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                    Http1Lifecycle::UpgradePending(Box::pin(tokio::time::sleep(server.config.max_operation_duration)))
                } else {
                    Http1Lifecycle::Active {
                        complete: head || response.body().is_end_stream(),
                        // An admitted reply runs to its operation's deadlines; any other keeps the exchange's.
                        control: control.filter(|_| !admitted),
                    }
                };
                drop(state);
                Ok::<_, io::Error>(response.map(|inner| Http1Body { inner, lifecycle }))
            }
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        let serving = builder
            .timer(TokioTimer::new())
            .header_read_timeout(None)
            .max_buf_size(MAX_HEADER_BYTES)
            .serve_connection(TokioIo::new(io), service)
            .with_upgrades();
        tokio::pin!(serving);
        tokio::select! {
            _ = stopped(stopping.clone()) => {
                serving.as_mut().graceful_shutdown();
                let _ = serving.await;
            },
            _ = &mut serving => {},
        }
        let pending = lock(&upgrade).take();
        if let Some(upgrade) = pending {
            upgrade.run().await;
        }
    }

    fn respond_authorized(
        &self,
        route: Route,
        request: Request<()>,
        peer: SocketAddr,
        owner: &Owner,
    ) -> Response<ResponseBody> {
        match route {
            _ if request.method() == Method::OPTIONS => empty_response(StatusCode::NO_CONTENT),
            Route::WtSession | Route::WsSession if self.auth.is_none() => {
                json_response(Bytes::from_static(br#"{"token":"","expires":0}"#))
            }
            Route::Download => self.download(&request, owner),
            Route::UploadSession | Route::UploadCheckpoint | Route::UploadProgress => {
                self.upload_control(route, &request, owner)
            }
            _ => match self.discovery.respond(route, &request, peer) {
                Ok(Some(response)) => response.map(ResponseBody::from),
                Ok(None) => text_response(StatusCode::NOT_FOUND),
                Err(_) => text_response(StatusCode::INTERNAL_SERVER_ERROR),
            },
        }
    }

    fn validate_request<B>(&self, request: &Request<B>, body_ended: bool) -> Option<Response<ResponseBody>> {
        // Hyper's read-buffer capacity can exceed its configured growth limit.
        // Check the parsed header size as well before executing any endpoint.
        let header_bytes = request.headers().iter().fold(
            request.method().as_str().len() + request.uri().to_string().len() + 14,
            |size, (name, value)| size + name.as_str().len() + value.as_bytes().len() + 4,
        );
        if header_bytes > MAX_HEADER_BYTES {
            return Some(text_response(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE));
        }
        // Like Go: a declared length, or an unknown one outside HTTP/3, is a body only POST may carry.
        let declared = request
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|length| length.to_str().ok()?.parse::<u64>().ok());
        let unknown = declared.is_none() && request.version() != http::Version::HTTP_3 && !body_ended;
        if request.method() != Method::POST && (declared.is_some_and(|length| length > 0) || unknown) {
            let mut response = text_body(StatusCode::BAD_REQUEST, "request body not accepted");
            if request.version() <= http::Version::HTTP_11 {
                response
                    .headers_mut()
                    .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
            }
            return Some(response);
        }
        None
    }

    fn refuse_route<B>(
        &self,
        request: &Request<B>,
        route: Option<Route>,
        lease: Option<&AuthLease>,
        peer: SocketAddr,
    ) -> Option<Response<ResponseBody>> {
        let path = request.uri().path();
        if path.contains('\\') || path.split('/').any(|part| matches!(part, "." | "..")) {
            return Some(text_response(StatusCode::NOT_FOUND));
        }
        let route = route?;
        if !allowed(route).any(|method| request.method() == method) {
            let mut allow: Vec<_> = allowed(route).collect();
            allow.sort_unstable();
            return Some(method_not_allowed(&allow.join(", ")));
        }
        if route.admission() != route::Admission::Unmetered
            && lease.is_none()
            && !client_address::resolve(peer, request.headers(), &self.config.trusted_proxies).usable
        {
            let mut response = text_body(StatusCode::BAD_REQUEST, "ambiguous client address");
            if self.auth.is_none() {
                Access::Public.apply_measurement(response.headers_mut());
            }
            return Some(response);
        }
        None
    }

    /// Go's `Enforce` and its mux's refusals, in their order, for every listener and WebTransport: the header and
    /// body limits, the policy and its preflights, then the route's methods and client evidence. `Err` is the gate's
    /// own answer; a request that passes keeps its authorization, lease and Origin.
    fn gate<B>(
        &self,
        request: Request<B>,
        accepted: Accepted,
        body_ended: bool,
    ) -> Result<(Checked<B>, Passed), Box<Response<ResponseBody>>> {
        if let Some(response) = self.validate_request(&request, body_ended) {
            return Err(Box::new(response));
        }
        // A route this listener does not mount is authorized first, as in Go, then answered 404.
        let route = route::lookup(request.uri().path()).filter(|&route| accepted.topology.mounts(route));
        let Some(auth) = &self.auth else {
            if let Some(response) = self.refuse_route(&request, route, None, accepted.peer) {
                return Err(Box::new(response));
            }
            let passed = Passed {
                route,
                lease: None,
                origin: None,
            };
            return Ok((Checked::Public(request), passed));
        };
        let authorized = auth
            .policy()
            .authorize(request, accepted.connection())
            .map_err(|rejected| {
                Box::new(self.auth_refusal(rejected.request(), rejected.reason(), accepted.connection()))
            })?;
        let lease = match authorized.authorization() {
            Authorization::Preflight(headers) => {
                let mut response = empty_response(StatusCode::NO_CONTENT);
                *response.headers_mut() = headers.clone();
                return Err(Box::new(self.harden(response)));
            }
            Authorization::Authenticated(lease) => Some(lease.clone()),
            Authorization::PublicAuth => None,
        };
        if let Some(response) = self.refuse_route(authorized.request(), route, lease.as_ref(), accepted.peer) {
            return Err(Box::new(self.harden(response)));
        }
        let origin = authorized.request().headers().get(header::ORIGIN).cloned();
        Ok((Checked::Authorized(authorized), Passed { route, lease, origin }))
    }

    async fn respond_incoming<B>(
        &self,
        request: Request<B>,
        accepted: Accepted,
        operations: &Operations,
        upgrade: Option<&Mutex<Option<websocket::Upgrade>>>,
    ) -> io::Result<Response<ResponseBody>>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let body_ended = request.body().is_end_stream();
        let (request, passed) = match self.gate(request, accepted, body_ended) {
            Ok(passed) => passed,
            Err(response) => return Ok(*response),
        };
        let request = match request {
            Checked::Authorized(authorized) if passed.controlled(authorized.request()) => {
                return self.control(authorized, passed, operations).await;
            }
            Checked::Authorized(authorized) => authorized.into_parts().0,
            Checked::Public(request) => request,
        };
        let Passed { route, lease, origin } = passed;
        let owner = self.owner(&request, lease.as_ref(), accepted.peer);
        let measurement = route.is_some();
        let upload = route == Some(Route::Upload) && request.method() == Method::POST;
        let guard = lease.clone();
        let dispatch = async {
            let Some(route) = route else {
                return Ok(self.app(&request, accepted.topology));
            };
            if route == Route::Ping && request.method() != Method::OPTIONS {
                return Ok(match upgrade {
                    Some(pending) => self.upgrade_websocket(request, &owner, lease.clone(), pending),
                    None => text_response(StatusCode::NOT_IMPLEMENTED),
                });
            }
            if route == Route::Upload && request.method() != Method::OPTIONS {
                self.receive_upload(request, &owner, operations).await
            } else {
                Ok(self.respond_authorized(route, request.map(|_| ()), accepted.peer, &owner))
            }
        };
        // An upload whose lease ends is answered with its revocation, which retains no operation: the ended lease
        // already ends the upload's own.
        let (mut response, revoked) = tokio::select! {
            biased;
            _ = lease_ended(guard) => {
                if !upload {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                for operation in lock(operations).iter() {
                    lock(operation).body_complete = true;
                }
                (upload::refusal(graphite_meter_core::failure::UploadRefusal::Revoked), true)
            },
            result = dispatch => (result?, false),
        };
        if measurement && let (Some(lease), Some(origin)) = (&lease, &origin) {
            lease.access(origin).apply_measurement(response.headers_mut());
        } else if measurement && self.auth.is_none() {
            Access::Public.apply_measurement(response.headers_mut());
        }
        if !revoked {
            self.retain_operation(&mut response, lease, operations);
        }
        Ok(self.harden(response))
    }

    /// The authentication controller's pages and the socket tickets. Each collects its bounded body within the
    /// control bound, and ends with the lease that authorized it.
    async fn control<B>(
        &self,
        authorized: AuthorizedRequest<B>,
        passed: Passed,
        operations: &Operations,
    ) -> io::Result<Response<ResponseBody>>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let auth = self.auth.as_ref().expect("an authorized request has a controller");
        let Passed { route, lease, origin } = passed;
        let logout = AuthRoute::lookup(authorized.request().method(), authorized.request().uri().path())
            == Some(AuthRoute::Logout);
        let execute = async {
            let authorized = authorized.try_map_body(collect_auth_body).await?;
            let mut response = auth.handle(&authorized).await.map(ResponseBody::from);
            let request = authorized.request();
            // The rest of an oversized body would read as the next request.
            if request.body().len() > crate::auth::http::FORM_BYTES && request.version() <= http::Version::HTTP_11 {
                response
                    .headers_mut()
                    .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
            }
            Ok::<_, io::Error>(response)
        };
        let mut response = tokio::select! {
            biased;
            _ = lease_ended(lease.clone()) => return Err(io::ErrorKind::PermissionDenied.into()),
            result = tokio::time::timeout(CONTROL, execute) => {
                result.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??
            },
        };
        // Socket tickets are measurement endpoints. Their authenticated
        // responses must be readable from the browser's approved origin,
        // including when the native listener uses a different port.
        if ticket(route)
            && let (Some(lease), Some(origin)) = (&lease, &origin)
        {
            lease.access(origin).apply_measurement(response.headers_mut());
        }
        // A successful logout deliberately revokes the current lease;
        // its cookie-clearing response must still reach the browser.
        if !(logout && response.status().is_redirection()) {
            self.retain_operation(&mut response, lease, operations);
        }
        Ok(response)
    }

    /// The browser app, for a path no route claims on a listener that serves it.
    fn app<B>(&self, request: &Request<B>, topology: topology::Topology) -> Response<ResponseBody> {
        if !topology.spa || auth_route::claims(request.uri().path()) {
            return text_response(StatusCode::NOT_FOUND);
        }
        let mut response = self
            .assets
            .serve(request.method(), request.uri().path(), request.headers())
            .map(ResponseBody::from);
        let Ok(sources) = self.discovery.page_sources(&crate::discovery::request_host(request)) else {
            return text_response(StatusCode::BAD_REQUEST);
        };
        let Ok(policy) = self.assets.page_policy(&sources).parse() else {
            return text_response(StatusCode::BAD_REQUEST);
        };
        let headers = response.headers_mut();
        headers.insert("content-security-policy", policy);
        headers.insert("x-frame-options", http::HeaderValue::from_static("DENY"));
        crate::auth::pages::harden(headers, self.auth.is_some());
        response
    }

    /// Go's Enforce sets these on every response once a request under authentication is known secure.
    fn harden(&self, mut response: Response<ResponseBody>) -> Response<ResponseBody> {
        if self.auth.is_some() {
            crate::auth::pages::harden(response.headers_mut(), true);
        }
        response
    }

    fn retain_operation(
        &self,
        response: &mut Response<ResponseBody>,
        lease: Option<AuthLease>,
        operations: &Operations,
    ) {
        if let Some(lease) = lease {
            let complete = response.body().is_end_stream();
            lock(
                response
                    .body_mut()
                    .operation
                    .get_or_insert_with(|| self.operation(None, complete)),
            )
            .revocation = Some(Box::pin(async move { lease.ended().await }));
        }
        if let Some(operation) = &response.body().operation {
            let mut operations = lock(operations);
            if !operations.iter().any(|entry| Arc::ptr_eq(entry, operation)) {
                operations.push(operation.clone());
            }
        }
    }

    /// Bounds a multiplexed request by its operations and counts it as admitted work once it holds a permit.
    /// Until then, as Go's boundedRequest, the whole exchange has the control bound, even if the peer withholds
    /// flow control.
    async fn guard(
        &self,
        operations: &Operations,
        work: &AdmittedWork,
        exchange: impl Future<Output = io::Result<()>>,
    ) -> io::Result<()> {
        let mut exchange = std::pin::pin!(exchange);
        let mut control = std::pin::pin!(tokio::time::sleep(CONTROL));
        let mut admitted = None;
        let guarded = std::future::poll_fn(|cx| {
            check_operations(operations, cx)?;
            let result = exchange.as_mut().poll(cx);
            if result.is_pending() {
                // Dispatch may install a lease in this poll; register its wake before flow control blocks.
                check_operations(operations, cx)?;
            }
            if admitted.is_none() && holds_permit(operations) {
                admitted = Some(work.admit());
            }
            if result.is_pending() && admitted.is_none() && control.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
            }
            result
        });
        tokio::time::timeout(self.config.max_operation_duration, guarded)
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }

    fn auth_refusal<B>(
        &self,
        request: &Request<B>,
        reason: crate::auth::policy::Refusal,
        connection: Connection,
    ) -> Response<ResponseBody> {
        let policy = self.auth.as_ref().expect("auth enabled").policy();
        let mut response = Response::new(ResponseBody::empty());
        *response.status_mut() = StatusCode::FORBIDDEN;
        *response.headers_mut() = crate::auth::pages::security_headers(None).expect("static auth CSP");
        let secure = policy.trust(request, connection.peer, connection.tls).secure;
        crate::auth::pages::harden(response.headers_mut(), secure);
        if reason == crate::auth::policy::Refusal::AuthenticationRequired {
            if request.version() <= http::Version::HTTP_11 {
                response
                    .headers_mut()
                    .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
            }
            let public = policy.public_origin();
            response
                .headers_mut()
                .insert("graphite-meter-auth", http::HeaderValue::from_static("required"));
            response
                .headers_mut()
                .insert("graphite-meter-browser-auth", http::HeaderValue::from_static("1"));
            let login: http::HeaderValue = format!("{public}{}", AuthRoute::Login.path())
                .parse()
                .expect("validated public origin");
            response.headers_mut().insert("graphite-meter-auth-url", login.clone());
            if connection.listener.ui && request.method() == Method::GET && request.uri().path() == "/" {
                self.auth
                    .as_ref()
                    .expect("auth enabled")
                    .debug(format_args!("unauthenticated UI root redirected to login"));
                *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
                response.headers_mut().insert(header::LOCATION, login);
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    http::HeaderValue::from_static("text/html; charset=utf-8"),
                );
                *response.body_mut() = Bytes::from(format!(
                    "<a href=\"{public}{}\">Temporary Redirect</a>.\n\n",
                    AuthRoute::Login.path()
                ))
                .into();
            }
            if let Some(origin) = request.headers().get(header::ORIGIN) {
                if origin == public {
                    Access::Cookie(origin).apply_response(response.headers_mut());
                } else if route::lookup(request.uri().path()).is_some()
                    && origin.to_str().is_ok_and(crate::auth::secure_browser_origin)
                {
                    Access::Bearer(origin).apply_response(response.headers_mut());
                }
            }
        }
        response
    }

    fn admit(&self, route: Route, owner: &Owner) -> Result<Permit, Box<Response<ResponseBody>>> {
        let session = route.admission() == route::Admission::Session;
        self.admission.acquire(session, owner.client_keys()).map_err(|refusal| {
            let mut response = text_response(StatusCode::from_u16(refusal.status()).expect("known status"));
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, http::HeaderValue::from_static("1"));
            Box::new(response)
        })
    }

    fn operation(&self, permit: Option<Permit>, body_complete: bool) -> Arc<Mutex<Operation>> {
        Arc::new(Mutex::new(Operation {
            permit,
            deadline: Box::pin(tokio::time::sleep(self.config.max_operation_duration)),
            body_complete,
            revocation: None,
            revoked: false,
        }))
    }

    fn download(&self, request: &Request<()>, owner: &Owner) -> Response<ResponseBody> {
        let permit = match self.admit(Route::Download, owner) {
            Ok(permit) => permit,
            Err(refusal) => return *refusal,
        };
        let count = download_bytes(request);
        let head = request.method() == Method::HEAD;
        let transfer = (count != 0 && request.method() == Method::GET)
            .then(|| self.download_meter.open())
            .flatten();
        let mut body = ResponseBody::download(self.download_block.clone(), if head { 0 } else { count }, transfer);
        body.operation = Some(self.operation(Some(permit), count == 0 || head));
        Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::CONTENT_LENGTH, count)
            .body(body)
            .expect("valid download headers")
    }
}

/// A request as the gate checked it; under authentication it keeps its authorization for the controller.
enum Checked<B> {
    Public(Request<B>),
    Authorized(AuthorizedRequest<B>),
}

impl<B> Checked<B> {
    fn into_request(self) -> Request<B> {
        match self {
            Self::Public(request) => request,
            Self::Authorized(authorized) => authorized.into_parts().0,
        }
    }
}

/// What the gate learned of a request it let through.
struct Passed {
    /// The route, when the listener mounts it.
    route: Option<Route>,
    /// Under authentication, the lease that ends the request's work when it ends.
    lease: Option<AuthLease>,
    /// The Origin a measurement answer's CORS names.
    origin: Option<http::HeaderValue>,
}

impl Passed {
    /// The controller answers its own paths and, as in Go, a ticket route where it is mounted, past the method check.
    fn controlled<B>(&self, request: &Request<B>) -> bool {
        auth_route::claims(request.uri().path()) || ticket(self.route)
    }
}

fn ticket(route: Option<Route>) -> bool {
    matches!(route, Some(Route::WsSession | Route::WtSession))
}

async fn lease_ended(lease: Option<AuthLease>) {
    match lease {
        Some(lease) => lease.ended().await,
        None => std::future::pending().await,
    }
}

/// Like Go's `MaxBytesReader`, collecting one byte past the limit marks the body oversized.
async fn collect_auth_body<B>(mut body: B) -> io::Result<Bytes>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut bytes = bytes::BytesMut::new();
    while bytes.len() <= crate::auth::http::FORM_BYTES
        && let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
    {
        if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
            bytes.extend_from_slice(&data[..data.len().min(crate::auth::http::FORM_BYTES + 1 - bytes.len())]);
        }
    }
    Ok(bytes.freeze())
}

fn download_bytes<B>(request: &Request<B>) -> u64 {
    query(request, "bytes")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .map_or(DEFAULT_DOWNLOAD_BYTES, |value| (value as u64).min(MAX_TRANSFER_BYTES))
}

/// Go's mux dispatches these: GET also serves HEAD, and plain HTTP routes answer OPTIONS.
fn allowed(route: Route) -> impl Iterator<Item = &'static str> {
    let methods = route.methods();
    methods
        .iter()
        .copied()
        .chain(methods.contains(&"GET").then_some("HEAD"))
        .chain((route.kind() == Kind::Http).then_some("OPTIONS"))
}

struct DeadlineIo<T> {
    inner: T,
    operations: Operations,
    lifecycle: Option<Arc<Mutex<Http1Lifecycle>>>,
}

enum Http1Lifecycle {
    Headers(Pin<Box<Sleep>>),
    /// `control` bounds the exchange from its request until it holds an admitted operation, as Go's
    /// boundedRequest does; an upload admitted while its body is read already owns the operation's deadlines.
    Active {
        complete: bool,
        control: Option<Pin<Box<Sleep>>>,
    },
    Idle(Pin<Box<Sleep>>),
    UpgradePending(Pin<Box<Sleep>>),
    Upgraded,
}

struct Http1Body {
    inner: ResponseBody,
    lifecycle: Arc<Mutex<Http1Lifecycle>>,
}

impl Body for Http1Body {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        if self.inner.is_end_stream() || matches!(result, Poll::Ready(None)) {
            let mut lifecycle = lock(&self.lifecycle);
            if let Http1Lifecycle::Active { complete, .. } = &mut *lifecycle {
                *complete = true;
            }
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<T> DeadlineIo<T> {
    fn check_deadlines(&self, cx: &mut Context<'_>) -> io::Result<()> {
        if let Some(lifecycle) = &self.lifecycle {
            let mut lifecycle = lock(lifecycle);
            let control = match &mut *lifecycle {
                Http1Lifecycle::Headers(deadline)
                | Http1Lifecycle::Idle(deadline)
                | Http1Lifecycle::UpgradePending(deadline) => {
                    if deadline.as_mut().poll(cx).is_ready() {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    false
                }
                Http1Lifecycle::Active {
                    control: Some(deadline),
                    ..
                } => deadline.as_mut().poll(cx).is_ready(),
                _ => false,
            };
            drop(lifecycle);
            if control && !holds_permit(&self.operations) {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
        check_operations(&self.operations, cx)
    }

    fn flushed(&self, cx: &mut Context<'_>) {
        if let Some(lifecycle) = &self.lifecycle {
            let mut lifecycle = lock(lifecycle);
            if matches!(*lifecycle, Http1Lifecycle::Active { complete: true, .. }) {
                *lifecycle = Http1Lifecycle::Idle(Box::pin(tokio::time::sleep(CONTROL)));
            } else if matches!(*lifecycle, Http1Lifecycle::UpgradePending(_)) {
                *lifecycle = Http1Lifecycle::Upgraded;
            }
            if let Http1Lifecycle::Idle(deadline) = &mut *lifecycle {
                let _ = deadline.as_mut().poll(cx);
            }
        }
        lock(&self.operations).retain(|operation| {
            let mut operation = lock(operation);
            if operation.body_complete {
                operation.permit.take();
                false
            } else {
                true
            }
        });
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for DeadlineIo<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        self.check_deadlines(cx)?;
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buffer);
        if buffer.filled().len() > before
            && let Some(lifecycle) = &self.lifecycle
        {
            let mut lifecycle = lock(lifecycle);
            if matches!(*lifecycle, Http1Lifecycle::Idle(_)) {
                *lifecycle = Http1Lifecycle::Headers(Box::pin(tokio::time::sleep(CONTROL)));
            }
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for DeadlineIo<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        self.check_deadlines(cx)?;
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.check_deadlines(cx)?;
        Pin::new(&mut self.inner).poll_write_vectored(cx, data)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_deadlines(cx)?;
        ready!(Pin::new(&mut self.inner).poll_flush(cx))?;
        self.flushed(cx);
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_deadlines(cx)?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn stopped(stopping: tokio::sync::watch::Sender<bool>) {
    let mut receiver = stopping.subscribe();
    if !*receiver.borrow_and_update() {
        let _ = receiver.changed().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A request through the whole pipeline, on a clear HTTP/1.1 connection of its own from `peer`.
    async fn respond_from(server: &HttpServer, peer: &str, method: Method, path: &str) -> Response<ResponseBody> {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "localhost:7246")
            .body(String::new())
            .unwrap();
        let accepted = Accepted {
            peer: peer.parse().unwrap(),
            ..h1()
        };
        let operations = Arc::new(Mutex::new(Vec::new()));
        server
            .respond_incoming(request, accepted, &operations, None)
            .await
            .unwrap()
    }

    async fn respond(server: &HttpServer, method: Method, path: &str) -> Response<ResponseBody> {
        respond_from(server, "127.0.0.1:31000", method, path).await
    }

    async fn next_data(body: &mut ResponseBody) -> Option<Bytes> {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)).await?;
        Some(frame.unwrap().into_data().unwrap())
    }

    fn h1() -> Accepted {
        Accepted {
            peer: "127.0.0.1:31000".parse().unwrap(),
            tls: false,
            topology: topology::tcp(NativeKind::H1, false).topology,
        }
    }

    struct UnreadBody;

    impl Body for UnreadBody {
        type Data = Bytes;
        type Error = io::Error;

        fn poll_frame(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            panic!("rejected upload method must not read the request body");
        }

        fn is_end_stream(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn measurement_methods_reject_work_before_touching_the_body_or_upload_store() {
        let mut config = Config::default();
        config.limits.sessions_per_client = 1;
        config.limits.operations_per_client = 1;
        let server = HttpServer::new(config.validated().unwrap()).unwrap();
        let id = server.uploads.mint().unwrap();

        let download = respond(&server, Method::POST, "/download?bytes=1048576").await;
        assert_eq!(download.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(download.headers()[header::ALLOW], "GET, HEAD, OPTIONS");
        let head = respond(&server, Method::HEAD, "/download?bytes=1048576").await;
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "1048576");
        assert!(head.body().is_end_stream());
        drop(head);
        let admitted = respond(&server, Method::GET, "/download?bytes=1").await;
        assert_eq!(admitted.status(), StatusCode::OK);
        for (method, path, allow) in [
            (Method::POST, "/download?bytes=1", "GET, HEAD, OPTIONS"),
            (Method::POST, "/probe", "GET, HEAD, OPTIONS"),
            (Method::DELETE, "/preflight", "GET, HEAD, OPTIONS"),
            (Method::POST, "/servers", "GET, HEAD, OPTIONS"),
            (Method::GET, "/upload/session", "OPTIONS, POST"),
            (Method::GET, "/upload/checkpoint", "OPTIONS, POST"),
            (Method::POST, "/upload/progress", "DELETE, GET, HEAD, OPTIONS"),
            (Method::HEAD, "/upload/progress", "GET, DELETE"),
        ] {
            let response = respond(&server, method, path).await;
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
            assert_eq!(response.headers()[header::ALLOW], allow, "{path}");
        }

        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("/upload?id={id}"))
            .body(UnreadBody)
            .unwrap();
        let response = server
            .respond_incoming(request, h1(), &Arc::new(Mutex::new(Vec::new())), None)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "OPTIONS, POST");
        assert_eq!(server.uploads.retained(), 0);
        drop(admitted);
    }

    #[tokio::test]
    async fn download_body_owns_capacity_and_options_does_not_consume_it() {
        let mut config = Config::default();
        config.limits.operations_per_client = 1;
        config.limits.sessions_per_client = 1;
        let server = HttpServer::new(config.validated().unwrap()).unwrap();
        let held = respond(&server, Method::GET, "/download?bytes=100").await;
        assert_eq!(held.status(), StatusCode::OK);
        assert_eq!(held.headers()[header::CONTENT_LENGTH], "100");

        let refused = respond(&server, Method::GET, "/download").await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(refused.headers()[header::RETRY_AFTER], "1");
        assert_eq!(refused.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

        let options = respond(&server, Method::OPTIONS, "/download").await;
        assert_eq!(options.status(), StatusCode::NO_CONTENT);
        drop(held);
        let download = respond(&server, Method::GET, "/download?bytes=0").await;
        assert_eq!(download.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn download_length_preserves_go_parsing_and_head_headers() {
        let server = HttpServer::new(Config::default().validated().unwrap()).unwrap();
        for (query, expected) in [
            ("", 25 * 1024 * 1024),
            ("bytes=0", 0),
            ("bytes=-1", 25 * 1024 * 1024),
            ("bytes=9223372036854775808", 25 * 1024 * 1024),
            ("bytes=9223372036854775807", 64_u64 * 1024 * 1024 * 1024),
            ("bytes=%2B123", 123),
            ("bytes=5&bytes=10", 5),
        ] {
            let response = respond(&server, Method::HEAD, &format!("/download?{query}")).await;
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                expected.to_string(),
                "{query}"
            );
            assert!(response.body().is_end_stream());
        }
    }

    #[tokio::test]
    async fn progress_claim_cancellation_owns_capacity_and_options_stays_unmetered() {
        let mut config = Config::default();
        config.limits.operations_per_client = 2;
        config.limits.sessions_per_client = 1;
        let server = HttpServer::new(config.validated().unwrap()).unwrap();
        let peer = "[2001:db8:1::1]:31000";
        let neighbor = "[2001:db8:1::2]:31000";
        let foreign = "[2001:db8:2::1]:31000";
        let mut minted = respond_from(&server, peer, Method::POST, "/upload/session").await;
        let data = next_data(minted.body_mut()).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&data).unwrap();
        let id = value["uploadId"].as_str().unwrap();
        let path = format!("/upload/progress?id={id}");
        let mut first = respond_from(&server, peer, Method::GET, &path).await;
        let ready = next_data(first.body_mut()).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&ready).unwrap()["type"],
            "ready"
        );
        let second = respond_from(&server, neighbor, Method::GET, &path).await;
        assert_eq!(second.status(), StatusCode::OK, "same IPv6 /64 shares upload ownership");
        let download = respond_from(&server, peer, Method::GET, "/download?bytes=1").await;
        assert_eq!(download.status(), StatusCode::TOO_MANY_REQUESTS);
        for path in ["/upload", "/upload/session", "/upload/checkpoint", "/upload/progress"] {
            let options = respond_from(&server, peer, Method::OPTIONS, path).await;
            assert_eq!(options.status(), StatusCode::NO_CONTENT);
        }
        let checkpoint = format!("/upload/checkpoint?id={id}");
        let refused = respond_from(&server, foreign, Method::POST, &checkpoint).await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(refused.headers()["x-graphite-upload-refusal"], "ownerMismatch");
        assert!(next_data(first.body_mut()).await.is_none());
        drop(first);
        let download = respond_from(&server, peer, Method::GET, "/download?bytes=1").await;
        assert_eq!(download.status(), StatusCode::OK);
        drop(second);
    }

    #[tokio::test]
    async fn last_frame_keeps_capacity_until_io_flush() {
        let mut config = Config::default();
        config.limits.operations_per_client = 1;
        config.limits.sessions_per_client = 1;
        let server = HttpServer::new(config.validated().unwrap()).unwrap();
        let request = Request::get("/download?bytes=1").body(String::new()).unwrap();
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut response = server.respond_incoming(request, h1(), &operations, None).await.unwrap();
        // The connection holds the reply's operation, as it did the moment the reply was answered.
        let mut io = DeadlineIo {
            inner: tokio::io::sink(),
            operations,
            lifecycle: None,
        };
        let frame = std::future::poll_fn(|cx| Pin::new(response.body_mut()).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        let data = frame.into_data().unwrap();
        drop(response);
        let download = async || respond(&server, Method::GET, "/download?bytes=1").await.status();
        assert_eq!(download().await, StatusCode::TOO_MANY_REQUESTS);
        io.write_all(&data).await.unwrap();
        assert_eq!(download().await, StatusCode::TOO_MANY_REQUESTS);
        io.flush().await.unwrap();
        assert_eq!(download().await, StatusCode::OK);
    }

    #[tokio::test]
    async fn last_frame_write_deadline_survives_body_drop() {
        let server = HttpServer::new(
            Config {
                max_operation_duration: Duration::from_millis(20),
                ..Config::default()
            }
            .validated()
            .unwrap(),
        )
        .unwrap();
        let request = Request::get("/download?bytes=2").body(String::new()).unwrap();
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut response = server.respond_incoming(request, h1(), &operations, None).await.unwrap();
        let (writer, _non_reading_peer) = tokio::io::duplex(1);
        let mut io = DeadlineIo {
            inner: writer,
            operations,
            lifecycle: None,
        };
        let frame = std::future::poll_fn(|cx| Pin::new(response.body_mut()).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        drop(response);
        let error = tokio::time::timeout(Duration::from_secs(1), io.write_all(&frame.into_data().unwrap()))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(server.admission.load().0, 1);
        drop(io);
        assert_eq!(server.admission.load().0, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn http1_control_exchanges_end_fifteen_seconds_after_they_start() {
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let accepted = h1();
        // A small pipe the peer drains a little at a time: the reply always moves, well within the write-stall bound.
        let (client, served) = tokio::io::duplex(16);
        let started = tokio::time::Instant::now();
        let serving = tokio::spawn(server.serve_http1_connection(served, accepted, None));
        let (mut reader, mut writer) = tokio::io::split(client);
        writer
            .write_all(b"GET /servers HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let trickle = tokio::spawn(async move {
            let (mut received, mut chunk) = (Vec::new(), [0; 16]);
            while let Ok(read @ 1..) = reader.read(&mut chunk).await {
                received.extend_from_slice(&chunk[..read]);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            received
        });
        tokio::time::timeout(Duration::from_secs(16), serving)
            .await
            .expect("a control reply outlived Go's fifteen seconds")
            .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(15));
        assert!(trickle.await.unwrap().starts_with(b"HTTP/1.1 200 OK\r\n"));
        drop(writer);
    }

    #[tokio::test(start_paused = true)]
    async fn partial_headers_receive_a_fresh_fifteen_second_deadline() {
        let (reader, mut peer) = tokio::io::duplex(64);
        let mut reader = DeadlineIo {
            inner: reader,
            operations: Arc::new(Mutex::new(Vec::new())),
            lifecycle: Some(Arc::new(Mutex::new(Http1Lifecycle::Idle(Box::pin(
                tokio::time::sleep(Duration::from_secs(15)),
            ))))),
        };
        tokio::time::advance(Duration::from_secs(14)).await;
        let partial = b"GET /probe HTTP/1.1\r\nHost:";
        peer.write_all(partial).await.unwrap();
        let mut received = vec![0; partial.len()];
        reader.read_exact(&mut received).await.unwrap();
        assert_eq!(received, partial);
        tokio::time::advance(Duration::from_secs(14)).await;
        let mut byte = [0];
        let read = reader.read(&mut byte);
        tokio::pin!(read);
        std::future::poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(read.await.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn upgrade_headers_keep_a_deadline_until_their_flush() {
        let lifecycle = Arc::new(Mutex::new(Http1Lifecycle::UpgradePending(Box::pin(
            tokio::time::sleep(Duration::from_millis(20)),
        ))));
        let (writer, _stopped_reader) = tokio::io::duplex(1);
        let mut writer = DeadlineIo {
            inner: writer,
            operations: Arc::new(Mutex::new(Vec::new())),
            lifecycle: Some(lifecycle),
        };
        let write = writer.write_all(b"HTTP/1.1 101 Switching Protocols");
        tokio::pin!(write);
        std::future::poll_fn(|cx| {
            assert!(write.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        tokio::time::advance(Duration::from_millis(20)).await;
        assert_eq!(write.await.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn flushed_upgrade_removes_http_idle_policy() {
        let lifecycle = Arc::new(Mutex::new(Http1Lifecycle::UpgradePending(Box::pin(
            tokio::time::sleep(Duration::from_millis(20)),
        ))));
        let mut writer = DeadlineIo {
            inner: tokio::io::sink(),
            operations: Arc::new(Mutex::new(Vec::new())),
            lifecycle: Some(lifecycle.clone()),
        };
        writer.write_all(b"HTTP/1.1 101 Switching Protocols").await.unwrap();
        writer.flush().await.unwrap();
        assert!(matches!(*lifecycle.lock().unwrap(), Http1Lifecycle::Upgraded));
        tokio::time::advance(Duration::from_secs(61)).await;
        writer.write_all(b"owned WebSocket frame").await.unwrap();
    }

    /// As Go grants every QUIC connection its window, a client's credit funds one on each QUIC connection it may
    /// hold. Password logins and grants all share one principal, so each of them is bounded alone.
    #[test]
    fn credit_funds_a_window_on_every_quic_connection_a_client_may_hold() {
        let mut config = Config {
            advertised_native: Some(Default::default()),
            ..Config::default()
        };
        config.public.both.push("self".into());
        config.auth.mode = AuthMode::Password;
        config.auth.public_url = "https://localhost".into();
        config.auth.password_hash =
            "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0".into();
        let server = HttpServer::new(config.validated().unwrap()).unwrap();
        let fund = |keys: &[String]| {
            (0..8)
                .map(|_| server.client_credit.claim(keys, QUIC_CREDIT_BYTES))
                .collect::<Option<Vec<_>>>()
        };
        let address = client_address::client_keys("192.0.2.1".parse().unwrap());
        let _address = fund(&address).expect("a window on each of an address's QUIC connections");
        assert!(
            server.client_credit.claim(&address, QUIC_CREDIT_BYTES).is_none(),
            "no window past them"
        );
        let _logins: Vec<_> = (0..3)
            .map(|session| {
                let owner = Owner::login("local-operator", &session.to_string());
                fund(owner.client_keys()).expect("a window on each QUIC connection of every password login")
            })
            .collect();
    }
}
