//! Shared endpoint state and an owned HTTP/1 connection loop.

mod http2;
mod http3;
mod quic;
pub(crate) mod response;
mod upload;
mod websocket;
mod webtransport;
pub use quic::QuicEndpoint;
use upload::ProgressBody;

use crate::{
    ServerError,
    admission::{Admission, Permit},
    auth::{
        AuthLease,
        policy::{Authorization, Connection, Listener},
    },
    budget::{self, DOWNLOAD_BLOCK_BYTES, H2_FLOOR_BYTES, QUIC_CREDIT_BYTES},
    client_address,
    config::{AuthMode, ConfigError, NativeKind, ValidatedConfig},
    connections::Connections,
    cors::Access,
    discovery::Discovery,
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
    net::TcpListener,
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
        let client_credit = budget::ClientCredit::new(
            bytes,
            config.max_connections,
            config.max_connections_per_client,
            QUIC_CREDIT_BYTES,
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
        let (h2, ui) = (
            kind == NativeKind::H2,
            matches!(kind, NativeKind::H1 | NativeKind::H1Tls),
        );
        // The HTTP/3 companion's probe advertises the QUIC port under the HTTP/3 public origin.
        let bootstrap = match self.config.listener(NativeKind::H3).public_origin.as_str() {
            _ if kind != NativeKind::H3 => None,
            "" => Some(listener.local_addr()?.port()),
            public => Some(
                graphite_meter_core::origin::target_origin(public)?
                    .ok_or("HTTP/3 public origin is missing")?
                    .port_number(),
            ),
        };
        let tls = tls.map(|tls| {
            let mut tls = (*tls).clone();
            tls.alpn_protocols = vec![if h2 { b"h2".to_vec() } else { b"http/1.1".to_vec() }];
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
                    tasks.spawn(async move {
                        let _permit = permit;
                        let _memory = memory;
                        let connection = Connection { peer, tls: tls.is_some(), listener: Listener { ui, webtransport: false } };
                        let Some(tls) = tls else {
                            return server.serve_http1_connection(socket, connection, None).await;
                        };
                        let stream = tokio::select! {
                            biased;
                            _ = stopped(server.stopping.clone()) => return,
                            result = tokio::time::timeout(CONTROL, tls.accept(socket).into_fallible()) => {
                                let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
                                match result {
                                    Ok(Ok(stream)) => stream,
                                    Ok(Err((error, _socket))) => {
                                        server.peers.write(format_args!("[gm:http] http: TLS handshake error from {peer}: {error}"));
                                        return;
                                    }
                                    Err(_) => {
                                        server.peers.write(format_args!("[gm:http] http: TLS handshake error from {peer}: timed out"));
                                        return;
                                    }
                                }
                            }
                        };
                        if !h2 {
                            server.serve_http1_connection(stream, connection, bootstrap).await;
                        } else if stream.get_ref().1.alpn_protocol() == Some(b"h2") {
                            server.serve_http2_connection(stream, connection).await;
                        }
                    });
                }
            }
        };
        self.stopping.send_replace(true);
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, async { while tasks.join_next().await.is_some() {} }).await;
        tasks.shutdown().await;
        result
    }

    async fn serve_http1_connection<T>(self: Arc<Self>, stream: T, connection: Connection, bootstrap_port: Option<u16>)
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
            let probe = request.uri().path() == "/probe" && request.method() != Method::OPTIONS;
            let lifecycle = lifecycle.clone();
            *lifecycle.lock().expect("HTTP/1 lifecycle poisoned") = Http1Lifecycle::Active {
                complete: false,
                control: Some(Box::pin(tokio::time::sleep(CONTROL))),
            };
            async move {
                let mut response = if bootstrap_port.is_some()
                    && !matches!(
                        request.uri().path(),
                        "/probe" | "/upload/session" | "/upload/checkpoint" | "/upload/progress" | "/wt/session"
                    ) {
                    text_response(StatusCode::NOT_FOUND)
                } else {
                    server
                        .respond_incoming(request, connection, &operations, Some(&pending_upgrade))
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
                    let mut operation = operation.lock().expect("operation poisoned");
                    operation.body_complete |= head;
                    operation.permit.is_some()
                });
                let mut state = lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
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
        let pending = upgrade.lock().expect("WebSocket upgrade poisoned").take();
        if let Some(upgrade) = pending {
            upgrade.run().await;
        }
    }

    pub fn respond(&self, request: Request<()>, peer: SocketAddr) -> Response<ResponseBody> {
        if let Some(response) = self.validate_request(&request, true) {
            return response;
        }
        if self.auth.is_some() {
            return text_response(StatusCode::FORBIDDEN);
        }
        let route = route::lookup(request.uri().path());
        if let Some(response) = self.refuse_route(&request, route, None, peer) {
            return response;
        }
        let owner = self.upload_owner(&request, peer);
        self.respond_authorized(request, peer, &owner)
    }

    fn respond_authorized(&self, request: Request<()>, peer: SocketAddr, owner: &Owner) -> Response<ResponseBody> {
        let path = request.uri().path();
        let mut response = if request.method() == Method::OPTIONS {
            empty_response(StatusCode::NO_CONTENT)
        } else if self.auth.is_none() && matches!(path, "/wt/session" | "/ws/session") {
            json_response(Bytes::from_static(br#"{"token":"","expires":0}"#))
        } else if path == "/download" {
            self.download(&request, owner)
        } else if path.starts_with("/upload") {
            self.upload_control(&request, owner)
        } else {
            match self.discovery.respond(&request, peer) {
                Ok(Some(response)) => response.map(ResponseBody::bytes),
                Ok(None) => text_response(StatusCode::NOT_FOUND),
                Err(_) => text_response(StatusCode::INTERNAL_SERVER_ERROR),
            }
        };
        if self.auth.is_none() {
            Access::Public.apply_measurement(response.headers_mut());
        }
        response
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

    async fn respond_incoming<B>(
        &self,
        request: Request<B>,
        connection: Connection,
        operations: &Operations,
        upgrade: Option<&Mutex<Option<websocket::Upgrade>>>,
    ) -> io::Result<Response<ResponseBody>>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        if let Some(response) = self.validate_request(&request, request.body().is_end_stream()) {
            return Ok(response);
        }
        // A route this listener does not mount is authorized first, as in Go, then answered 404.
        let route = route::lookup(request.uri().path()).filter(|&route| mounts(connection.listener, route));
        let mut lease = None;
        let mut origin = None;
        let request = if let Some(auth) = &self.auth {
            let authorized = match auth.policy().authorize(request, connection) {
                Ok(authorized) => authorized,
                Err(rejected) => {
                    return Ok(self.auth_refusal(rejected.request(), rejected.reason(), connection));
                }
            };
            if let Authorization::Preflight(headers) = authorized.authorization() {
                let mut response = Response::new(ResponseBody::empty());
                *response.status_mut() = StatusCode::NO_CONTENT;
                *response.headers_mut() = headers.clone();
                return Ok(self.harden(response));
            }
            if let Authorization::Authenticated(guard) = authorized.authorization() {
                lease = Some(guard.clone());
            }
            origin = authorized.request().headers().get(header::ORIGIN).cloned();
            if let Some(response) = self.refuse_route(authorized.request(), route, lease.as_ref(), connection.peer) {
                return Ok(self.harden(response));
            }
            let path = authorized.request().uri().path();
            // As in Go, a ticket route reaches the controller only where it is mounted, past the method check.
            let ticket = matches!(route, Some(Route::WsSession | Route::WtSession));
            if path == "/login" || path.starts_with("/auth/") || ticket {
                let logout = path == "/auth/logout" && authorized.request().method() == Method::POST;
                // Even public auth endpoints collect only their form's bytes, within the control bound.
                let execute = async {
                    let authorized = authorized.try_map_body(collect_auth_body).await?;
                    let mut response = match auth.handle(&authorized).await {
                        Some(response) => response.map(ResponseBody::bytes),
                        None => text_response(StatusCode::NOT_FOUND),
                    };
                    let request = authorized.request();
                    // The rest of an oversized body would read as the next request.
                    if request.body().len() > crate::auth::http::FORM_BYTES
                        && request.version() <= http::Version::HTTP_11
                    {
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
                if ticket && let (Some(lease), Some(origin)) = (&lease, &origin) {
                    lease.access(origin).apply_measurement(response.headers_mut());
                }
                // A successful logout deliberately revokes the current lease;
                // its cookie-clearing response must still reach the browser.
                if !(logout && response.status().is_redirection()) {
                    self.retain_operation(&mut response, lease, operations);
                }
                return Ok(response);
            }
            authorized.into_parts().0
        } else if let Some(response) = self.refuse_route(&request, route, None, connection.peer) {
            return Ok(response);
        } else {
            request
        };
        let owner = lease
            .as_ref()
            .map_or_else(|| self.upload_owner(&request, connection.peer), AuthLease::owner);
        let measurement = route.is_some();
        let upload = route == Some(Route::Upload) && request.method() == Method::POST;
        let guard = lease.clone();
        let dispatch = async {
            if route.is_none() {
                let path = request.uri().path();
                if !connection.listener.ui || path == "/login" || path.starts_with("/auth/") {
                    return Ok(text_response(StatusCode::NOT_FOUND));
                }
                let mut response = self
                    .assets
                    .serve(request.method(), request.uri().path(), request.headers())
                    .map(ResponseBody::bytes);
                let Ok(sources) = self.discovery.page_sources(&crate::discovery::request_host(&request)) else {
                    return Ok(text_response(StatusCode::BAD_REQUEST));
                };
                let Ok(policy) = self.assets.page_policy(&sources).parse() else {
                    return Ok(text_response(StatusCode::BAD_REQUEST));
                };
                let headers = response.headers_mut();
                headers.insert("content-security-policy", policy);
                headers.insert("x-frame-options", http::HeaderValue::from_static("DENY"));
                crate::auth::pages::harden(headers, self.auth.is_some());
                return Ok(response);
            }
            if route == Some(Route::Ping) && request.method() != Method::OPTIONS {
                return Ok(match upgrade {
                    Some(pending) => self.upgrade_websocket(request, &owner, lease.clone(), pending),
                    None => text_response(StatusCode::NOT_IMPLEMENTED),
                });
            }
            if route == Some(Route::Upload) && request.method() != Method::OPTIONS {
                self.receive_upload(request, &owner, operations).await
            } else {
                Ok(self.respond_authorized(request.map(|_| ()), connection.peer, &owner))
            }
        };
        let mut response = tokio::select! {
            biased;
            _ = lease_ended(guard) => {
                if upload {
                    for operation in operations.lock().expect("operations poisoned").iter() {
                        operation.lock().expect("operation poisoned").body_complete = true;
                    }
                    upload::refusal(graphite_meter_core::failure::UploadRefusal::Revoked)
                } else {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
            },
            result = dispatch => result?,
        };
        if measurement && let (Some(lease), Some(origin)) = (&lease, &origin) {
            lease.access(origin).apply_measurement(response.headers_mut());
        } else if measurement && self.auth.is_none() {
            Access::Public.apply_measurement(response.headers_mut());
        }
        if response
            .headers()
            .get("x-graphite-upload-refusal")
            .is_none_or(|code| code != "revoked")
        {
            self.retain_operation(&mut response, lease, operations);
        }
        Ok(self.harden(response))
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
            response
                .body_mut()
                .operation
                .get_or_insert_with(|| self.operation(None, complete))
                .lock()
                .expect("operation poisoned")
                .revocation = Some(Box::pin(async move { lease.ended().await }));
        }
        if let Some(operation) = &response.body().operation {
            let mut operations = operations.lock().expect("operations poisoned");
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
            response.headers_mut().insert(
                "graphite-meter-auth-url",
                format!("{public}/login").parse().expect("validated public origin"),
            );
            if connection.listener.ui && request.method() == Method::GET && request.uri().path() == "/" {
                self.auth
                    .as_ref()
                    .expect("auth enabled")
                    .debug(format_args!("unauthenticated UI root redirected to login"));
                *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
                response.headers_mut().insert(
                    header::LOCATION,
                    format!("{public}/login").parse().expect("validated public origin"),
                );
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    http::HeaderValue::from_static("text/html; charset=utf-8"),
                );
                *response.body_mut() =
                    ResponseBody::bytes(format!("<a href=\"{public}/login\">Temporary Redirect</a>.\n\n").into());
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
        let mut body = ResponseBody {
            block: self.download_block.clone(),
            remaining: count,
            progress: None,
            transfer: (count != 0 && request.method() == Method::GET)
                .then(|| self.download_meter.open())
                .flatten(),
            operation: Some(self.operation(Some(permit), count == 0 || request.method() == Method::HEAD)),
        };
        if request.method() == Method::HEAD {
            body.remaining = 0;
        }
        Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::CONTENT_LENGTH, count)
            .body(body)
            .expect("valid download headers")
    }
}

fn mounts(listener: Listener, route: Route) -> bool {
    match route.kind() {
        Kind::WebTransport => listener.webtransport,
        _ => {
            listener.ui
                || !matches!(
                    route,
                    Route::Preflight | Route::Servers | Route::WsSession | Route::Ping
                )
        }
    }
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

impl Operation {
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.revoked
            || self
                .revocation
                .as_mut()
                .is_some_and(|ended| ended.as_mut().poll(cx).is_ready())
        {
            self.revoked = true;
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if self.deadline.as_mut().poll(cx).is_ready() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
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

/// Direct callers own capacity through this body. A listener additionally holds
/// the operation until its final bytes flush, or the connection is dropped.
/// Chunks share a single immutable random block instead of allocating per write.
pub struct ResponseBody {
    block: Bytes,
    remaining: u64,
    progress: Option<ProgressBody>,
    transfer: Option<crate::meter::Transfer>,
    operation: Option<Arc<Mutex<Operation>>>,
}

impl From<Bytes> for ResponseBody {
    fn from(block: Bytes) -> Self {
        Self::bytes(block)
    }
}

impl ResponseBody {
    fn empty() -> Self {
        Self::bytes(Bytes::new())
    }
    fn bytes(block: Bytes) -> Self {
        Self {
            remaining: block.len() as u64,
            block,
            progress: None,
            transfer: None,
            operation: None,
        }
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.is_end_stream() {
            return Poll::Ready(None);
        }
        if let Some(operation) = &self.operation {
            let error = operation.lock().expect("operation poisoned").check(cx).err();
            if let Some(error) = error {
                self.remaining = 0;
                return Poll::Ready(Some(Err(error)));
            }
        }
        if let Some(progress) = &mut self.progress {
            let frame = progress.poll_frame(cx);
            let done = progress.done;
            if done && let Some(operation) = &self.operation {
                operation.lock().expect("operation poisoned").body_complete = true;
            }
            return frame;
        }
        let length = self.remaining.min(self.block.len() as u64) as usize;
        self.remaining -= length as u64;
        if let Some(transfer) = &self.transfer {
            transfer.record(length);
        }
        if self.remaining == 0
            && let Some(operation) = &self.operation
        {
            operation.lock().expect("operation poisoned").body_complete = true;
        }
        Poll::Ready(Some(Ok(Frame::data(self.block.slice(..length)))))
    }

    fn is_end_stream(&self) -> bool {
        self.progress
            .as_ref()
            .map_or(self.remaining == 0, |progress| progress.done)
    }
    fn size_hint(&self) -> SizeHint {
        if self.progress.as_ref().is_some_and(|progress| !progress.done) {
            SizeHint::default()
        } else {
            SizeHint::with_exact(self.remaining)
        }
    }
}

// An operation outlives the body when Hyper has queued its last frame but has
// not flushed it. Keeping both deadline and permit here bounds stalled writes.
struct Operation {
    permit: Option<Permit>,
    deadline: Pin<Box<Sleep>>,
    body_complete: bool,
    revocation: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    revoked: bool,
}

type Operations = Arc<Mutex<Vec<Arc<Mutex<Operation>>>>>;

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
            let mut lifecycle = self.lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
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
            let mut lifecycle = lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
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
            let mut lifecycle = lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
            if matches!(*lifecycle, Http1Lifecycle::Active { complete: true, .. }) {
                *lifecycle = Http1Lifecycle::Idle(Box::pin(tokio::time::sleep(CONTROL)));
            } else if matches!(*lifecycle, Http1Lifecycle::UpgradePending(_)) {
                *lifecycle = Http1Lifecycle::Upgraded;
            }
            if let Http1Lifecycle::Idle(deadline) = &mut *lifecycle {
                let _ = deadline.as_mut().poll(cx);
            }
        }
        self.operations
            .lock()
            .expect("connection operations poisoned")
            .retain(|operation| {
                let mut operation = operation.lock().expect("operation poisoned");
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
            let mut lifecycle = lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
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

fn check_operations(operations: &Operations, cx: &mut Context<'_>) -> io::Result<()> {
    for operation in operations.lock().expect("operations poisoned").iter() {
        operation.lock().expect("operation poisoned").check(cx)?;
    }
    Ok(())
}

/// Admitted operations on one connection; leftover receive credit is reclaimed the control bound after the last.
#[derive(Clone)]
struct AdmittedWork(Arc<Mutex<WorkState>>);

struct WorkState {
    running: usize,
    idle_since: tokio::time::Instant,
}

impl AdmittedWork {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(WorkState {
            running: 0,
            idle_since: tokio::time::Instant::now(),
        })))
    }

    fn admit(&self) -> Admitted {
        self.0.lock().expect("admitted work poisoned").running += 1;
        Admitted(self.clone())
    }

    fn idle_since(&self) -> Option<tokio::time::Instant> {
        let work = self.0.lock().expect("admitted work poisoned");
        (work.running == 0).then_some(work.idle_since)
    }
}

struct Admitted(AdmittedWork);

impl Drop for Admitted {
    fn drop(&mut self) {
        let mut work = self.0.0.lock().expect("admitted work poisoned");
        work.running -= 1;
        if work.running == 0 {
            work.idle_since = tokio::time::Instant::now();
        }
    }
}

fn holds_permit(operations: &Operations) -> bool {
    let operations = operations.lock().expect("operations poisoned");
    operations
        .iter()
        .any(|operation| operation.lock().expect("operation poisoned").permit.is_some())
}

/// The client keys an admitted exchange holds its permit under, which also bound its receive credit.
fn admitted_clients(operations: &Operations) -> Option<Vec<String>> {
    let operations = operations.lock().expect("operations poisoned");
    operations.iter().find_map(|operation| {
        let operation = operation.lock().expect("operation poisoned");
        operation.permit.as_ref().map(|permit| permit.clients().to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        let peer = "127.0.0.1:31000".parse().unwrap();
        let id = server.uploads.mint().unwrap();

        let download = server.respond(
            Request::builder()
                .method(Method::POST)
                .uri("/download?bytes=1048576")
                .body(())
                .unwrap(),
            peer,
        );
        assert_eq!(download.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(download.headers()[header::ALLOW], "GET, HEAD, OPTIONS");
        let head = server.respond(
            Request::builder()
                .method(Method::HEAD)
                .uri("/download?bytes=1048576")
                .body(())
                .unwrap(),
            peer,
        );
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "1048576");
        assert!(head.body().is_end_stream());
        drop(head);
        let _admitted = server.respond(Request::get("/download?bytes=1").body(()).unwrap(), peer);
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
            let request = Request::builder().method(method).uri(path).body(()).unwrap();
            let response = server.respond(request, peer);
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
            assert_eq!(response.headers()[header::ALLOW], allow, "{path}");
        }

        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("/upload?id={id}"))
            .body(UnreadBody)
            .unwrap();
        let connection = Connection {
            peer,
            tls: false,
            listener: Listener {
                ui: true,
                webtransport: false,
            },
        };
        let response = server
            .respond_incoming(request, connection, &Arc::new(Mutex::new(Vec::new())), None)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "OPTIONS, POST");
        assert_eq!(server.uploads.retained(), 0);
    }

    #[tokio::test]
    async fn last_frame_keeps_capacity_until_io_flush() {
        let mut config = Config::default();
        config.limits.operations_per_client = 1;
        config.limits.sessions_per_client = 1;
        let server = HttpServer::new(config.validated().unwrap()).unwrap();
        let peer = "127.0.0.1:31000".parse().unwrap();
        let request = || Request::builder().uri("/download?bytes=1").body(()).unwrap();
        let mut response = server.respond(request(), peer);
        let operations = Arc::new(Mutex::new(vec![response.body().operation.clone().unwrap()]));
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
        assert_eq!(server.respond(request(), peer).status(), StatusCode::TOO_MANY_REQUESTS);
        io.write_all(&data).await.unwrap();
        assert_eq!(server.respond(request(), peer).status(), StatusCode::TOO_MANY_REQUESTS);
        io.flush().await.unwrap();
        assert_eq!(server.respond(request(), peer).status(), StatusCode::OK);
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
        let peer = "127.0.0.1:31000".parse().unwrap();
        let request = Request::builder().uri("/download?bytes=2").body(()).unwrap();
        let mut response = server.respond(request, peer);
        let operations = Arc::new(Mutex::new(vec![response.body().operation.clone().unwrap()]));
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
        let connection = Connection {
            peer: "127.0.0.1:31000".parse().unwrap(),
            tls: false,
            listener: Listener {
                ui: true,
                webtransport: false,
            },
        };
        // A small pipe the peer drains a little at a time: the reply always moves, well within the write-stall bound.
        let (client, served) = tokio::io::duplex(16);
        let started = tokio::time::Instant::now();
        let serving = tokio::spawn(server.serve_http1_connection(served, connection, None));
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
}
