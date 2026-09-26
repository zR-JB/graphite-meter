//! Shared endpoint state and an owned HTTP/1 connection loop.

#[path = "http_h2.rs"]
mod http_h2;
#[path = "http_h3.rs"]
mod http_h3;
#[path = "http_quic.rs"]
mod http_quic;
#[path = "http_wt.rs"]
mod http_wt;
pub use http_h3::{Http3RequestKind, Http3RequestStream};
#[path = "http_websocket.rs"]
mod http_websocket;
#[path = "http_upload.rs"]
mod upload_http;
use upload_http::ProgressBody;

use crate::{
    admission::{Admission, Class, Permit},
    auth::{
        AuthLease,
        policy::{Authorization, Connection, Listener},
    },
    client_address,
    config::{AuthMode, Config, ConfigError},
    connections::Connections,
    cors::Access,
    discovery::Discovery,
    upload::Owner,
    upload::UploadStore,
};
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode, header};
use hyper::{
    body::{Body, Frame, SizeHint},
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpListener,
    task::JoinSet,
    time::Sleep,
};

#[derive(Clone, Copy)]
enum HttpProtocol {
    Http1,
    Http2,
    Bootstrap(u16),
}

const MAX_HEADER_BYTES: usize = 32 * 1024;
const DEFAULT_DOWNLOAD_BYTES: u64 = 25 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024 * 1024;

pub struct HttpServer {
    config: Arc<Config>,
    discovery: Discovery,
    admission: Admission,
    connections: Connections,
    quic_send_budget: Arc<tokio::sync::Semaphore>,
    download_block: Bytes,
    uploads: UploadStore,
    auth: Option<crate::auth::http::Service>,
    assets: crate::assets::Assets,
    app_security: crate::app_security::AppSecurity,
}

impl HttpServer {
    pub async fn initialize_auth(&self) -> Result<(), ConfigError> {
        if let Some(auth) = &self.auth {
            auth.initialize().await?;
        }
        Ok(())
    }

    pub fn new(config: Arc<Config>) -> Result<Self, ConfigError> {
        config.validate()?;
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
        let app_security =
            crate::app_security::AppSecurity::new(config.clone(), assets.inline_script_hash())?;
        let admission = Admission::new(config.limits);
        let discovery = Discovery::new(config.clone(), Some(admission.clone()), None)?;
        let connections = Connections::new(
            config.max_connections,
            config.max_connections_per_client,
            config.trusted_proxies.clone(),
        );
        let mut block = vec![0; 256 * 1024];
        getrandom::fill(&mut block).map_err(|_| "download payload randomness unavailable")?;
        Ok(Self {
            config,
            discovery,
            admission,
            connections,
            quic_send_budget: Arc::new(tokio::sync::Semaphore::new(http_quic::SEND_WINDOW_BUDGET)),
            download_block: block.into(),
            uploads: UploadStore::new()?,
            auth,
            assets,
            app_security,
        })
    }

    /// The caller owns listener binding and shutdown. No connection task escapes
    /// this scope, including on accept errors or server cancellation.
    pub async fn serve_http1(
        self: Arc<Self>,
        listener: TcpListener,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ConfigError> {
        self.serve_tcp(listener, None, HttpProtocol::Http1, shutdown)
            .await
    }

    /// TLS setup is supplied by the caller; connection capacity covers both the
    /// bounded handshake and the complete HTTP connection lifetime.
    pub async fn serve_https1(
        self: Arc<Self>,
        listener: TcpListener,
        tls: Arc<rustls::ServerConfig>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ConfigError> {
        if tls.alpn_protocols != [b"http/1.1".to_vec()] {
            return Err("HTTP/1 TLS listener requires an http/1.1-only ALPN configuration".into());
        }
        self.serve_tcp(
            listener,
            Some(tokio_rustls::TlsAcceptor::from(tls)),
            HttpProtocol::Http1,
            shutdown,
        )
        .await
    }

    /// HTTP/2 uses stream-local cancellation; one slow stream never sets a
    /// deadline on the shared socket. This listener requires negotiated h2.
    pub async fn serve_http2(
        self: Arc<Self>,
        listener: TcpListener,
        tls: Arc<rustls::ServerConfig>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ConfigError> {
        if tls.alpn_protocols != [b"h2".to_vec()] {
            return Err("HTTP/2 listener requires an h2-only TLS ALPN configuration".into());
        }
        self.serve_tcp(
            listener,
            Some(tokio_rustls::TlsAcceptor::from(tls)),
            HttpProtocol::Http2,
            shutdown,
        )
        .await
    }

    /// TCP companion on the HTTP/3 UDP port. It serves only the authenticated
    /// probe/OPTIONS route and never gains UI or transfer authority.
    pub async fn serve_https_bootstrap(
        self: Arc<Self>,
        listener: TcpListener,
        tls: Arc<rustls::ServerConfig>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ConfigError> {
        if tls.alpn_protocols != [b"http/1.1".to_vec()] {
            return Err("HTTP/3 bootstrap requires an http/1.1-only TLS ALPN configuration".into());
        }
        let public = &self
            .config
            .listener(crate::config::NativeKind::H3)
            .public_origin;
        let port = if public.is_empty() {
            listener.local_addr()?.port()
        } else {
            url::Url::parse(public)?
                .port_or_known_default()
                .ok_or("HTTP/3 public origin has no effective port")?
        };
        self.serve_tcp(
            listener,
            Some(tokio_rustls::TlsAcceptor::from(tls)),
            HttpProtocol::Bootstrap(port),
            shutdown,
        )
        .await
    }

    async fn serve_tcp(
        self: Arc<Self>,
        listener: TcpListener,
        tls: Option<tokio_rustls::TlsAcceptor>,
        protocol: HttpProtocol,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ConfigError> {
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
                            accept_delay = (accept_delay * 2)
                                .clamp(Duration::from_millis(5), Duration::from_secs(1));
                            accept_at = tokio::time::Instant::now() + accept_delay;
                            continue;
                        }
                    };
                    accept_delay = Duration::ZERO;
                    let Ok(permit) = self.connections.acquire(peer) else {
                        continue;
                    };
                    // Small control replies must not wait for Nagle buffering.
                    let _ = socket.set_nodelay(true);
                    #[cfg(target_os = "linux")]
                    if matches!(protocol, HttpProtocol::Http2) {
                        let _ = socket2::SockRef::from(&socket).set_tcp_notsent_lowat(64 * 1024);
                    }
                    let server = self.clone();
                    let tls = tls.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        if let Some(tls) = tls {
                            if let Ok(Ok(stream)) = tokio::time::timeout(
                                Duration::from_secs(10), tls.accept(socket),
                            ).await {
                                let connection = Connection {
                                    peer,
                                    tls: true,
                                    listener: Listener {
                                        ui: matches!(protocol, HttpProtocol::Http1),
                                        webtransport: false,
                                    },
                                };
                                match protocol {
                                    HttpProtocol::Http1 => {
                                        server.serve_http1_connection(stream, connection, None).await;
                                    }
                                    HttpProtocol::Bootstrap(port) => {
                                        server.serve_http1_connection(stream, connection, Some(port)).await;
                                    }
                                    HttpProtocol::Http2 if stream.get_ref().1.alpn_protocol() == Some(b"h2") => {
                                        server.serve_http2_connection(stream, connection).await;
                                    }
                                    HttpProtocol::Http2 => {}
                                }
                            }
                        } else {
                            let connection = Connection {
                                peer,
                                tls: false,
                                listener: Listener { ui: true, webtransport: false },
                            };
                            server.serve_http1_connection(socket, connection, None).await;
                        }
                    });
                }
            }
        };
        tasks.shutdown().await;
        result
    }

    async fn serve_http1_connection<T>(
        self: Arc<Self>,
        stream: T,
        connection: Connection,
        bootstrap_port: Option<u16>,
    ) where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let upgrade = Arc::new(Mutex::new(None));
        let pending_upgrade = upgrade.clone();
        let lifecycle = Arc::new(Mutex::new(Http1Lifecycle::Headers(Box::pin(
            tokio::time::sleep(Duration::from_secs(10)),
        ))));
        // Wrap the TLS stream, not its raw socket: a successful flush must also
        // drain encrypted records before releasing the response's capacity.
        let io = DeadlineIo {
            inner: stream,
            operations: operations.clone(),
            lifecycle: Some(lifecycle.clone()),
        };
        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
            let server = self.clone();
            let operations = operations.clone();
            let pending_upgrade = pending_upgrade.clone();
            let head = request.method() == Method::HEAD;
            let lifecycle = lifecycle.clone();
            *lifecycle.lock().expect("HTTP/1 lifecycle poisoned") =
                Http1Lifecycle::Active { complete: false };
            async move {
                let mut response = if bootstrap_port.is_some() && request.uri().path() != "/probe" {
                    text_response(StatusCode::NOT_FOUND)
                } else {
                    server
                        .respond_incoming(request, connection, &operations, Some(&pending_upgrade))
                        .await?
                };
                if let Some(port) = bootstrap_port
                    && response.status().is_success()
                {
                    response.headers_mut().insert(
                        header::ALT_SVC,
                        format!("h3=\":{port}\"").parse().expect("valid port"),
                    );
                    response
                        .headers_mut()
                        .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
                }
                if head && let Some(operation) = &response.body().auth_operation {
                    operation.lock().expect("operation poisoned").body_complete = true;
                }
                if let Some(operation) = &response.body().operation {
                    if head {
                        operation.lock().expect("operation poisoned").body_complete = true;
                    }
                    let mut pending = operations.lock().expect("connection operations poisoned");
                    if !pending.iter().any(|entry| Arc::ptr_eq(entry, operation)) {
                        pending.push(operation.clone());
                    }
                }
                *lifecycle.lock().expect("HTTP/1 lifecycle poisoned") =
                    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                        Http1Lifecycle::UpgradePending(Box::pin(tokio::time::sleep(
                            server.config.max_operation_duration,
                        )))
                    } else {
                        Http1Lifecycle::Active {
                            complete: head || response.body().is_end_stream(),
                        }
                    };
                Ok::<_, io::Error>(response.map(|inner| Http1Body { inner, lifecycle }))
            }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .timer(TokioTimer::new())
            // Hyper's header timer also covers keepalive waiting. The IO
            // lifecycle below separates 60s idle from 10s partial headers.
            // Hyper exposes no unread-buffer hook: a partial pipelined header
            // prefetched during the preceding request can retain the 60s bound
            // until another socket read, rather than Go's 10s header bound.
            .header_read_timeout(None)
            .max_buf_size(MAX_HEADER_BYTES)
            .serve_connection(TokioIo::new(io), service)
            .with_upgrades()
            .await;
        let pending = upgrade.lock().expect("WebSocket upgrade poisoned").take();
        if let Some(upgrade) = pending {
            upgrade.run().await;
        }
    }

    pub fn respond(&self, request: Request<()>, peer: SocketAddr) -> Response<ResponseBody> {
        if self.auth.is_some() {
            return text_response(StatusCode::FORBIDDEN);
        }
        let owner = self.upload_owner(&request, peer);
        self.respond_authorized(request, peer, &owner)
    }

    fn respond_authorized(
        &self,
        request: Request<()>,
        peer: SocketAddr,
        owner: &Owner,
    ) -> Response<ResponseBody> {
        if let Some(response) = self.validate_request(&request) {
            return response;
        }
        let path = request.uri().path();
        let mut response = if request.method() == Method::OPTIONS {
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(ResponseBody::empty())
                .expect("static response")
        } else if self.auth.is_none() && matches!(path, "/wt/session" | "/ws/session") {
            if request.method() == Method::POST {
                Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(ResponseBody::bytes(Bytes::from_static(
                        br#"{"token":"","expires":0}"#,
                    )))
                    .expect("static public socket session")
            } else {
                text_response(StatusCode::METHOD_NOT_ALLOWED)
            }
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

    fn validate_request<B>(&self, request: &Request<B>) -> Option<Response<ResponseBody>> {
        // Hyper's read-buffer capacity can exceed its configured growth limit.
        // Check the parsed header size as well before executing any endpoint.
        let header_bytes = request.headers().iter().fold(
            request.method().as_str().len() + request.uri().to_string().len() + 14,
            |size, (name, value)| size + name.as_str().len() + value.as_bytes().len() + 4,
        );
        if header_bytes > MAX_HEADER_BYTES {
            return Some(text_response(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE));
        }
        let path = request.uri().path();
        if path.contains('\\') || path.split('/').any(|part| matches!(part, "." | "..")) {
            return Some(text_response(StatusCode::NOT_FOUND));
        }
        None
    }

    async fn respond_incoming<B>(
        &self,
        request: Request<B>,
        connection: Connection,
        operations: &Operations,
        upgrade: Option<&Mutex<Option<http_websocket::Upgrade>>>,
    ) -> io::Result<Response<ResponseBody>>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        if let Some(response) = self.validate_request(&request) {
            return Ok(response);
        }
        if !connection.listener.ui
            && matches!(
                request.uri().path(),
                "/preflight" | "/servers" | "/ws/session" | "/ws/ping"
            )
        {
            return Ok(text_response(StatusCode::NOT_FOUND));
        }
        let mut lease = None;
        let mut origin = None;
        let request = if let Some(auth) = &self.auth {
            let authorized = match auth.policy().authorize(request, connection) {
                Ok(authorized) => authorized,
                Err(rejected) => {
                    return Ok(self.auth_refusal(
                        rejected.request(),
                        rejected.reason(),
                        connection,
                    ));
                }
            };
            if let Authorization::Preflight(headers) = authorized.authorization() {
                let mut response = Response::new(ResponseBody::empty());
                *response.status_mut() = StatusCode::NO_CONTENT;
                *response.headers_mut() = headers.clone();
                return Ok(response);
            }
            if let Authorization::Authenticated(guard) = authorized.authorization() {
                lease = Some(guard.clone());
            }
            origin = authorized.request().headers().get(header::ORIGIN).cloned();
            let path = authorized.request().uri().path();
            if path == "/login"
                || path.starts_with("/auth/")
                || matches!(path, "/ws/session" | "/wt/session")
            {
                let logout =
                    path == "/auth/logout" && authorized.request().method() == Method::POST;
                let ticket = matches!(path, "/ws/session" | "/wt/session");
                // Even public auth endpoints collect only 4096 bytes within 15s.
                let execute = async {
                    let authorized = authorized.try_map_body(collect_auth_body).await?;
                    let response = auth.handle(&authorized).await.unwrap_or_else(|| {
                        Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Bytes::new())
                            .unwrap()
                    });
                    Ok::<_, io::Error>(response.map(ResponseBody::bytes))
                };
                let mut response = tokio::select! {
                    biased;
                    _ = lease_ended(lease.clone()) => return Err(io::ErrorKind::PermissionDenied.into()),
                    result = tokio::time::timeout(Duration::from_secs(15), execute) => {
                        result.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??
                    },
                };
                // Socket tickets are measurement endpoints. Their authenticated
                // responses must be readable from the browser's approved origin,
                // including when the native listener uses a different port.
                if ticket && let (Some(lease), Some(origin)) = (&lease, &origin) {
                    if lease.is_bearer() {
                        Access::Bearer(origin)
                    } else {
                        Access::Cookie(origin)
                    }
                    .apply_measurement(response.headers_mut());
                }
                // A successful logout deliberately revokes the current lease;
                // its cookie-clearing response must still reach the browser.
                if !(logout && response.status().is_redirection()) {
                    self.retain_auth(&mut response, lease, operations);
                }
                return Ok(response);
            }
            authorized.into_parts().0
        } else {
            request
        };
        let owner = lease.as_ref().map_or_else(
            || self.upload_owner(&request, connection.peer),
            AuthLease::owner,
        );
        let measurement = graphite_meter_core::route::lookup(request.uri().path()).is_some();
        let guard = lease.clone();
        let dispatch = async {
            if graphite_meter_core::route::lookup(request.uri().path()).is_none() {
                if !connection.listener.ui {
                    return Ok(text_response(StatusCode::NOT_FOUND));
                }
                let authority = request
                    .uri()
                    .authority()
                    .map(|authority| authority.as_str())
                    .or_else(|| {
                        request
                            .headers()
                            .get(header::HOST)
                            .and_then(|host| host.to_str().ok())
                    })
                    .unwrap_or_default();
                let mut response = self
                    .assets
                    .serve(request.method(), request.uri().path())
                    .map(ResponseBody::bytes);
                match self.app_security.headers(authority) {
                    Ok(headers) => response.headers_mut().extend(headers),
                    Err(_) => return Ok(text_response(StatusCode::BAD_REQUEST)),
                }
                return Ok(response);
            }
            if request.uri().path() == "/ws/ping" && request.method() != Method::OPTIONS {
                return Ok(match upgrade {
                    Some(pending) => {
                        self.upgrade_websocket(request, &owner, lease.clone(), pending)
                    }
                    None => text_response(StatusCode::NOT_IMPLEMENTED),
                });
            }
            if request.uri().path() == "/upload" && request.method() != Method::OPTIONS {
                self.receive_upload(request, &owner, operations).await
            } else {
                Ok(self.respond_authorized(request.map(|_| ()), connection.peer, &owner))
            }
        };
        let mut response = tokio::select! {
            biased;
            _ = lease_ended(guard) => return Err(io::ErrorKind::PermissionDenied.into()),
            result = dispatch => result?,
        };
        if measurement && let (Some(lease), Some(origin)) = (&lease, &origin) {
            if lease.is_bearer() {
                Access::Bearer(origin)
            } else {
                Access::Cookie(origin)
            }
            .apply_measurement(response.headers_mut());
        } else if measurement && self.auth.is_none() {
            Access::Public.apply_measurement(response.headers_mut());
        }
        self.retain_auth(&mut response, lease, operations);
        Ok(response)
    }

    fn retain_auth(
        &self,
        response: &mut Response<ResponseBody>,
        lease: Option<AuthLease>,
        operations: &Operations,
    ) {
        if let Some(lease) = lease {
            let operation = Arc::new(Mutex::new(Operation {
                permit: None,
                deadline: Box::pin(tokio::time::sleep(self.config.max_operation_duration)),
                body_complete: response.body().is_end_stream(),
                revocation: Some(Box::pin(async move { lease.ended().await })),
                revoked: false,
            }));
            operations
                .lock()
                .expect("operations poisoned")
                .push(operation.clone());
            response.body_mut().auth_operation = Some(operation);
        }
    }

    fn auth_refusal<B>(
        &self,
        request: &Request<B>,
        reason: crate::auth::policy::Refusal,
        connection: Connection,
    ) -> Response<ResponseBody> {
        let mut response = text_response(StatusCode::FORBIDDEN);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-store"),
        );
        if matches!(
            request.version(),
            http::Version::HTTP_09 | http::Version::HTTP_10 | http::Version::HTTP_11
        ) {
            response
                .headers_mut()
                .insert(header::CONNECTION, http::HeaderValue::from_static("close"));
        }
        if reason == crate::auth::policy::Refusal::AuthenticationRequired {
            let public = self
                .auth
                .as_ref()
                .expect("auth enabled")
                .policy()
                .public_origin();
            response.headers_mut().insert(
                "graphite-meter-auth",
                http::HeaderValue::from_static("required"),
            );
            response.headers_mut().insert(
                "graphite-meter-browser-auth",
                http::HeaderValue::from_static("1"),
            );
            response.headers_mut().insert(
                "graphite-meter-auth-url",
                format!("{public}/login")
                    .parse()
                    .expect("validated public origin"),
            );
            if connection.listener.ui
                && request.method() == Method::GET
                && request.uri().path() == "/"
            {
                *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
                response.headers_mut().insert(
                    header::LOCATION,
                    format!("{public}/login")
                        .parse()
                        .expect("validated public origin"),
                );
            }
            if let Some(origin) = request.headers().get(header::ORIGIN) {
                if origin == public {
                    Access::Cookie(origin).apply_response(response.headers_mut());
                } else if graphite_meter_core::route::lookup(request.uri().path()).is_some()
                    && origin.to_str().ok().is_some_and(|raw| {
                        raw.starts_with("https://")
                            && graphite_meter_core::origin::canonical_origin(raw)
                                .is_ok_and(|canonical| canonical == raw)
                    })
                {
                    Access::Bearer(origin).apply_response(response.headers_mut());
                }
            }
        }
        response
    }

    fn download(&self, request: &Request<()>, owner: &Owner) -> Response<ResponseBody> {
        let permit = match self.admission.acquire(Class::Request, owner.budget_key()) {
            Ok(permit) => permit,
            Err(refusal) => {
                let mut response =
                    text_response(StatusCode::from_u16(refusal.status()).expect("known status"));
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, http::HeaderValue::from_static("1"));
                return response;
            }
        };
        if request.method() != Method::GET && request.method() != Method::HEAD {
            return method_not_allowed("GET, HEAD");
        }
        let count = download_bytes(request.uri().query().unwrap_or_default());
        let mut body = ResponseBody {
            block: self.download_block.clone(),
            remaining: count,
            progress: None,
            auth_operation: None,
            operation: Some(Arc::new(Mutex::new(Operation {
                permit: Some(permit),
                deadline: Box::pin(tokio::time::sleep(self.config.max_operation_duration)),
                revocation: None,
                revoked: false,
                body_complete: count == 0 || request.method() == Method::HEAD,
            }))),
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

async fn lease_ended(lease: Option<AuthLease>) {
    match lease {
        Some(lease) => lease.ended().await,
        None => std::future::pending().await,
    }
}

async fn collect_auth_body<B>(mut body: B) -> io::Result<Bytes>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut bytes = bytes::BytesMut::new();
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
            if data.len() > 4096 - bytes.len() {
                return Err(io::ErrorKind::InvalidData.into());
            }
            bytes.extend_from_slice(&data);
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

fn download_bytes(query: &str) -> u64 {
    let value = form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "bytes")
        .map(|(_, value)| value);
    value
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
        .map_or(DEFAULT_DOWNLOAD_BYTES, |value| {
            (value as u64).min(MAX_DOWNLOAD_BYTES)
        })
}

fn text_response(status: StatusCode) -> Response<ResponseBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("x-content-type-options", "nosniff")
        .body(ResponseBody::bytes(Bytes::from(format!(
            "{}\n",
            status.canonical_reason().unwrap_or("error")
        ))))
        .expect("static error response")
}

fn method_not_allowed(allow: &'static str) -> Response<ResponseBody> {
    let mut response = text_response(StatusCode::METHOD_NOT_ALLOWED);
    response
        .headers_mut()
        .insert(header::ALLOW, http::HeaderValue::from_static(allow));
    response
}

/// Direct callers own capacity through this body. A listener additionally holds
/// the operation until its final bytes flush, or the connection is dropped.
/// Chunks share a single immutable random block instead of allocating per write.
pub struct ResponseBody {
    block: Bytes,
    remaining: u64,
    progress: Option<ProgressBody>,
    operation: Option<Arc<Mutex<Operation>>>,
    auth_operation: Option<Arc<Mutex<Operation>>>,
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
            auth_operation: None,
            operation: None,
        }
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.is_end_stream() {
            return Poll::Ready(None);
        }
        if let Some(operation) = &self.auth_operation
            && operation
                .lock()
                .expect("operation poisoned")
                .check(cx)
                .is_err()
        {
            return Poll::Ready(Some(Err(io::ErrorKind::PermissionDenied.into())));
        }
        if let Some(operation) = &self.operation
            && operation
                .lock()
                .expect("operation poisoned")
                .deadline
                .as_mut()
                .poll(cx)
                .is_ready()
        {
            self.remaining = 0;
            return Poll::Ready(Some(Err(io::ErrorKind::TimedOut.into())));
        }
        if let Some(progress) = &mut self.progress {
            let frame = progress.poll_frame(cx);
            let done = progress.done;
            if done && let Some(operation) = &self.operation {
                operation.lock().expect("operation poisoned").body_complete = true;
            }
            if done && let Some(operation) = &self.auth_operation {
                operation.lock().expect("operation poisoned").body_complete = true;
            }
            return frame;
        }
        let length = self.remaining.min(self.block.len() as u64) as usize;
        self.remaining -= length as u64;
        if self.remaining == 0
            && let Some(operation) = &self.operation
        {
            operation.lock().expect("operation poisoned").body_complete = true;
        }
        if self.remaining == 0
            && let Some(operation) = &self.auth_operation
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
        if self
            .progress
            .as_ref()
            .is_some_and(|progress| !progress.done)
        {
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
    Active { complete: bool },
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

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        if self.inner.is_end_stream() || matches!(result, Poll::Ready(None)) {
            let mut lifecycle = self.lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
            if let Http1Lifecycle::Active { complete } = &mut *lifecycle {
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
            if let Http1Lifecycle::Headers(deadline)
            | Http1Lifecycle::Idle(deadline)
            | Http1Lifecycle::UpgradePending(deadline) = &mut *lifecycle
                && deadline.as_mut().poll(cx).is_ready()
            {
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
        for operation in self
            .operations
            .lock()
            .expect("connection operations poisoned")
            .iter()
        {
            operation.lock().expect("operation poisoned").check(cx)?;
        }
        Ok(())
    }

    fn flushed(&self, cx: &mut Context<'_>) {
        if let Some(lifecycle) = &self.lifecycle {
            let mut lifecycle = lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
            if matches!(*lifecycle, Http1Lifecycle::Active { complete: true }) {
                *lifecycle =
                    Http1Lifecycle::Idle(Box::pin(tokio::time::sleep(Duration::from_secs(60))));
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
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check_deadlines(cx)?;
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buffer);
        if buffer.filled().len() > before
            && let Some(lifecycle) = &self.lifecycle
        {
            let mut lifecycle = lifecycle.lock().expect("HTTP/1 lifecycle poisoned");
            if matches!(*lifecycle, Http1Lifecycle::Idle(_)) {
                *lifecycle =
                    Http1Lifecycle::Headers(Box::pin(tokio::time::sleep(Duration::from_secs(10))));
            }
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for DeadlineIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    struct UnreadBody;

    impl Body for UnreadBody {
        type Data = Bytes;
        type Error = io::Error;

        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            panic!("rejected upload method must not read the request body");
        }
    }

    #[tokio::test]
    async fn measurement_methods_reject_work_before_touching_the_body_or_upload_store() {
        let server = HttpServer::new(Arc::new(Config::default())).unwrap();
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
        assert_eq!(download.headers()[header::ALLOW], "GET, HEAD");
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

        let uri = format!("/upload?id={id}");
        let direct = server.respond(
            Request::builder()
                .method(Method::GET)
                .uri(&uri)
                .body(())
                .unwrap(),
            peer,
        );
        assert_eq!(direct.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(direct.headers()[header::ALLOW], "POST");
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(UnreadBody)
            .unwrap();
        let owner = server.upload_owner(&request, peer);
        let response = server
            .receive_upload(request, &owner, &Arc::new(Mutex::new(Vec::new())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "POST");
        assert_eq!(server.uploads.retained(), 0);
    }

    #[tokio::test]
    async fn last_frame_keeps_capacity_until_io_flush() {
        let mut config = Config::default();
        config.limits.operations_per_client = 1;
        config.limits.sessions_per_client = 1;
        let server = HttpServer::new(Arc::new(config)).unwrap();
        let peer = "127.0.0.1:31000".parse().unwrap();
        let request = || {
            Request::builder()
                .uri("/download?bytes=1")
                .body(())
                .unwrap()
        };
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
        assert_eq!(
            server.respond(request(), peer).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        io.write_all(&data).await.unwrap();
        assert_eq!(
            server.respond(request(), peer).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        io.flush().await.unwrap();
        assert_eq!(server.respond(request(), peer).status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn last_frame_write_deadline_survives_body_drop() {
        let server = HttpServer::new(Arc::new(Config {
            max_operation_duration: Duration::from_millis(20),
            ..Config::default()
        }))
        .unwrap();
        let peer = "127.0.0.1:31000".parse().unwrap();
        let request = Request::builder()
            .uri("/download?bytes=2")
            .body(())
            .unwrap();
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
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            io.write_all(&frame.into_data().unwrap()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(server.admission.load().0, 1);
        drop(io);
        assert_eq!(server.admission.load().0, 0);
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
        writer
            .write_all(b"HTTP/1.1 101 Switching Protocols")
            .await
            .unwrap();
        writer.flush().await.unwrap();
        assert!(matches!(
            *lifecycle.lock().unwrap(),
            Http1Lifecycle::Upgraded
        ));
        tokio::time::advance(Duration::from_secs(61)).await;
        writer.write_all(b"owned WebSocket frame").await.unwrap();
    }
}
