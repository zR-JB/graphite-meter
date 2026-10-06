//! Connections over HTTP/1.1, HTTP/2 and HTTP/3, QUIC dialing, WebTransport sessions, bodies and answers.
use super::{CONTROL_TIMEOUT, Client, Request, fault::Fault};
use bytes::Bytes;
use futures_util::FutureExt;
use graphite_meter_http3::{
    self as http3, Code, client,
    webtransport::{self, RecvStream, SendStream},
};
use graphite_meter_net::{ConnectError, RequestForm, Verify, bind_udp, client_config, quic::client_transport, resolve};
use graphite_meter_proto::{
    discovery::{MAX_RESPONSE_BYTES, Protocol},
    lane::LaneEnding,
    origin::{Host, Origin, Scheme},
    route::Route,
};
use http::{HeaderValue, Method, Version, header};
use http_body_util::BodyExt;
use hyper::{
    body::{Frame, SizeHint},
    client::conn::{http1, http2},
};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use noq::crypto::rustls::QuicClientConfig;
use std::{
    convert::Infallible,
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    runtime::Handle,
    task::{AbortHandle, JoinHandle},
    time::timeout,
};

/// Receive windows that let one connection carry 5 Gbit/s over a 100 ms path.
const H2_STREAM_WINDOW: u32 = 32 << 20;
const H2_CONNECTION_WINDOW: u32 = 64 << 20;
/// The largest DATA frame accepted, as the server sends them: larger frames cost less CPU per byte.
const H2_FRAME_BYTES: u32 = 64 << 10;
/// An HTTP/2 connection that reads nothing this long is pinged, and closed when the ping goes unanswered.
const H2_KEEP_ALIVE: Duration = Duration::from_secs(30);
const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);
/// A download lane's HTTP/1.1 reads: hyper's adaptive buffer grows past it and costs memory.
const FIXED_READ_BYTES: usize = 256 << 10;
/// The most bytes one transfer request or stream carries.
pub(super) const TRANSFER_BYTES: u64 = 64 << 30;
/// How long a server that stopped reading an upload may take to show its answer.
const EARLY_ANSWER: Duration = Duration::from_secs(1);

/// How an HTTP/1.1 connection reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBuffer {
    Adaptive,
    /// A fixed 256 KiB buffer, as download lanes read.
    Fixed,
}

/// Reads a JSON answer, as `Preflight::decode` and its siblings do.
pub type Decode<T> = fn(&[u8]) -> Result<T, serde_json::Error>;

/// One connection to an origin.
pub enum Conn {
    Http1 { sender: http1::SendRequest<Payload>, form: RequestForm },
    Http2(http2::SendRequest<Payload>),
    Http3 { requests: http3::client::SendRequest, quic: Arc<Quic> },
}

pub(super) type Answer = http::Response<Incoming>;

/// A request that failed; `again` when its connection failed before answering, so it may go on another.
pub(super) struct Failed {
    pub fault: Fault,
    pub again: bool,
}

impl Conn {
    /// A connection to `origin` over `via`, HTTP/2 if TLS agrees; TCP runs where dialed, QUIC on `home` or a runtime.
    pub(super) async fn dial(
        client: &Client,
        origin: &Origin,
        via: Protocol,
        buffer: ReadBuffer,
        home: Option<&Handle>,
    ) -> Result<Self, Fault> {
        if via == Protocol::Http3 {
            let home = home.cloned().unwrap_or_else(|| client.shared.runtimes.next());
            let (quic, requests) = dial(origin, client.shared.verify, &home).await?;
            return Ok(Self::Http3 { requests, quic });
        }
        let connection = client.shared.connector.connect(origin, Some(via)).await?;
        let h2 = connection.form == RequestForm::Origin
            && match connection.alpn.as_deref() {
                Some(alpn) => alpn == b"h2",
                None => origin.scheme == Scheme::Http && via == Protocol::Http2,
            };
        if via == Protocol::Http2 && !h2 {
            return Err(Fault::Malformed("server or proxy does not support HTTP/2".into()));
        }
        let io = TokioIo::new(connection.stream);
        if !h2 {
            let mut builder = http1::Builder::new();
            if buffer == ReadBuffer::Fixed {
                builder.read_buf_exact_size(Some(FIXED_READ_BYTES));
            }
            let (sender, driver) = builder.handshake(io).await.map_err(|error| hyper_fault(&error))?;
            tokio::spawn(async move { drop(driver.with_upgrades().await) });
            return Ok(Self::Http1 { sender, form: connection.form });
        }
        let (sender, driver) = http2::Builder::new(TokioExecutor::new())
            .timer(TokioTimer::new())
            .max_frame_size(H2_FRAME_BYTES)
            .initial_stream_window_size(H2_STREAM_WINDOW)
            .initial_connection_window_size(H2_CONNECTION_WINDOW)
            .keep_alive_interval(H2_KEEP_ALIVE)
            .keep_alive_timeout(H2_KEEP_ALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .handshake(io)
            .await
            .map_err(|error| hyper_fault(&error))?;
        tokio::spawn(async move { drop(driver.await) });
        Ok(Self::Http2(sender))
    }

    /// Another handle to a multiplexed connection; none for HTTP/1.1.
    pub(super) fn share(&self) -> Option<Self> {
        match self {
            Self::Http1 { .. } => None,
            Self::Http2(sender) => Some(Self::Http2(sender.clone())),
            Self::Http3 { requests, quic } => Some(Self::Http3 { requests: requests.clone(), quic: quic.clone() }),
        }
    }

    /// Whether the connection may take another request.
    pub(super) fn usable(&self) -> bool {
        match self {
            Self::Http1 { sender, .. } => !sender.is_closed(),
            Self::Http2(sender) => !sender.is_closed(),
            Self::Http3 { requests, quic } => !quic.closed() && !requests.going_away(),
        }
    }

    /// Whether an HTTP/1.1 connection finished its last exchange.
    pub(super) fn idle(&self) -> bool {
        matches!(self, Self::Http1 { sender, .. } if sender.is_ready())
    }

    /// The runtime a QUIC connection runs on.
    pub(super) fn home(&self) -> Option<Handle> {
        match self {
            Self::Http3 { quic, .. } => Some(quic.home.clone()),
            _ => None,
        }
    }

    /// Sends `request` with `body` and reads its answer's head, failing unless it is a 200.
    pub async fn open(&mut self, client: &Client, request: &Request, body: Payload) -> Result<Incoming, Fault> {
        let mut head = client.head(request)?;
        if body.remaining > 0 {
            let headers = head.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
            headers.insert(header::CONTENT_LENGTH, body.remaining.into());
        }
        let answer = self.send(head, body).await.map_err(|failed| failed.fault)?;
        client.check(request, answer)
    }

    /// Sends a request with an absolute URI and `body`, and reads its answer's head.
    pub(super) async fn send(&mut self, mut head: http::Request<()>, body: Payload) -> Result<Answer, Failed> {
        let response = match self {
            Self::Http1 { sender, form } => {
                http1_form(&mut head, form);
                sender.ready().await.map_err(hyper_failed)?;
                sender.send_request(head.map(|()| body)).await
            }
            Self::Http2(sender) => {
                sender.ready().await.map_err(hyper_failed)?;
                sender.send_request(head.map(|()| body)).await
            }
            Self::Http3 { requests, quic } => return http3_send(requests, quic, head, body).await,
        };
        let incoming = |body| Incoming { source: Source::Hyper(body), ended: None };
        Ok(response.map_err(hyper_failed)?.map(incoming))
    }
}

/// An HTTP/1.1 head: in origin form with its Host, or in absolute form with a proxy's credentials.
fn http1_form(head: &mut http::Request<()>, form: &RequestForm) {
    let authority = head.uri().authority();
    if let Some(Ok(host)) = authority.map(|authority| HeaderValue::from_str(authority.as_str())) {
        head.headers_mut().insert(header::HOST, host);
    }
    match form {
        RequestForm::Origin => {
            let path = head.uri().path_and_query().map_or("/", |path| path.as_str());
            *head.uri_mut() = path.parse().unwrap_or_default();
        }
        RequestForm::Absolute { authorization: Some(authorization) } => {
            if let Ok(value) = HeaderValue::from_str(authorization) {
                head.headers_mut().insert(header::PROXY_AUTHORIZATION, value);
            }
        }
        RequestForm::Absolute { authorization: None } => {}
    }
    *head.version_mut() = Version::HTTP_11;
}

/// Sends `body` in slices; a server that stopped reading it shows its answer instead.
async fn http3_send(
    requests: &http3::client::SendRequest,
    quic: &Arc<Quic>,
    head: http::Request<()>,
    mut body: Payload,
) -> Result<Answer, Failed> {
    let (mut send, mut recv) = requests.send_request(head).await.map_err(http3_failed)?.split();
    let sent = async {
        while let Some(slice) = body.slice() {
            send.send_data(slice).await?;
        }
        send.finish().await
    };
    let response = match sent.await {
        Ok(()) => recv.response().await.map_err(http3_failed)?,
        Err(error) => match tokio::time::timeout(EARLY_ANSWER, recv.response()).await {
            Ok(Ok(response)) => response,
            _ => return Err(http3_failed(error)),
        },
    };
    let source = Source::Http3 { recv: Box::new(recv), _quic: quic.clone() };
    Ok(response.map(|()| Incoming { source, ended: None }))
}

/// A request body: none, one block once, or one block repeated for a transfer's bytes.
pub struct Payload {
    block: Bytes,
    remaining: u64,
    /// Counts the bytes handed on.
    sent: Option<Arc<AtomicU64>>,
}

impl Payload {
    pub fn empty() -> Self {
        Self { block: Bytes::new(), remaining: 0, sent: None }
    }

    /// `bytes` once.
    pub fn once(bytes: Bytes) -> Self {
        Self { remaining: bytes.len() as u64, block: bytes, sent: None }
    }

    /// `block` repeated for a transfer's bytes, adding each slice handed on to `sent`.
    pub fn repeat(block: Bytes, sent: Arc<AtomicU64>) -> Self {
        Self { block, remaining: TRANSFER_BYTES, sent: Some(sent) }
    }

    /// The next slice, `None` once the transfer's bytes were handed on.
    pub(super) fn slice(&mut self) -> Option<Bytes> {
        let size = self.remaining.min(self.block.len() as u64);
        if size == 0 {
            return None;
        }
        self.remaining -= size;
        if let Some(sent) = &self.sent {
            sent.fetch_add(size, Ordering::Relaxed);
        }
        Some(self.block.slice(..size as usize))
    }
}

impl hyper::body::Body for Payload {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(self.get_mut().slice().map(|slice| Ok(Frame::data(slice))))
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

/// A response body, holding the connection it reads from.
pub struct Incoming {
    source: Source,
    /// Runs once the body ended.
    ended: Option<Box<dyn FnOnce() + Send>>,
}

enum Source {
    Hyper(hyper::body::Incoming),
    Http3 { recv: Box<http3::RecvHalf>, _quic: Arc<Quic> },
}

impl Incoming {
    /// The next payload, or `None` at the body's end; an HTTP/3 body cut short of its length ends there.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Fault> {
        self.read(true).await
    }

    /// The same body, running `ended` once it ended.
    pub(super) fn then(self, ended: impl FnOnce() + Send + 'static) -> Self {
        Self { ended: Some(Box::new(ended)), ..self }
    }

    /// The next payload; `lenient` reads a message error at an HTTP/3 body's end as its end.
    async fn read(&mut self, lenient: bool) -> Result<Option<Bytes>, Fault> {
        let read = match &mut self.source {
            Source::Hyper(body) => loop {
                let Some(frame) = body.frame().await else { break Ok(None) };
                if let Ok(data) = frame.map_err(|error| hyper_fault(&error))?.into_data() {
                    break Ok(Some(data));
                }
            },
            Source::Http3 { recv, .. } => match recv.data().await {
                Err(http3::Error::Protocol(Code::H3_MESSAGE_ERROR)) if lenient => Ok(None),
                data => data.map_err(http3_fault),
            },
        };
        if let Ok(None) = read
            && let Some(ended) = self.ended.take()
        {
            ended();
        }
        read
    }

    /// The body, at most 64 KiB of it, read by `decode`.
    pub async fn json<T>(mut self, decode: Decode<T>) -> Result<T, Fault> {
        let oversized = || Fault::Malformed(format!("control response exceeds {MAX_RESPONSE_BYTES} bytes"));
        if let Source::Hyper(body) = &self.source
            && hyper::body::Body::size_hint(body).exact() > Some(MAX_RESPONSE_BYTES as u64)
        {
            return Err(oversized());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = self.read(false).await? {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(oversized());
            }
            bytes.extend_from_slice(&chunk);
        }
        decode(&bytes).map_err(|error| Fault::Malformed(error.to_string()))
    }
}

fn hyper_fault(error: &hyper::Error) -> Fault {
    match () {
        _ if error.is_parse() => Fault::Malformed(error.to_string()),
        _ if error.is_timeout() => Fault::TimedOut("HTTP exchange"),
        _ => Fault::Lost(error.to_string()),
    }
}

/// A hyper failure; a stream the peer reset ends alone, so only a connection's own failure goes again.
fn hyper_failed(error: hyper::Error) -> Failed {
    let source = std::error::Error::source(&error).and_then(|source| source.downcast_ref::<h2::Error>());
    let again = !error.is_user() && !source.is_some_and(h2::Error::is_reset);
    Failed { fault: hyper_fault(&error), again }
}

pub(super) fn http3_fault(error: http3::Error) -> Fault {
    match error {
        http3::Error::Protocol(_) | http3::Error::Connection { local: true, .. } => Fault::Malformed(error.to_string()),
        http3::Error::TimedOut => Fault::TimedOut("HTTP/3 exchange"),
        error => Fault::Lost(error.to_string()),
    }
}

fn http3_failed(error: http3::Error) -> Failed {
    let again = matches!(
        error,
        http3::Error::Transport(_) | http3::Error::Connection { local: false, .. } | http3::Error::GoingAway
    );
    Failed { fault: http3_fault(error), again }
}

/// How long an address may stay silent: lost handshake packets on a lossy path take seconds.
const SILENT: Duration = Duration::from_secs(3);
/// How long an answering address may take to finish the handshake.
const ANSWERING: Duration = Duration::from_secs(5);

/// A QUIC connection with its endpoint and HTTP/3 driver; dropping it closes them.
pub struct Quic {
    connection: noq::Connection,
    _endpoint: noq::Endpoint,
    driver: AbortHandle,
    /// The runtime that runs them.
    pub home: Handle,
}

impl Quic {
    pub fn closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }
}

impl Drop for Quic {
    fn drop(&mut self) {
        self.connection.close(Code::H3_NO_ERROR.into(), b"");
        self.driver.abort();
    }
}

/// Dials `origin` on `runtime`, which then runs the connection, endpoint and driver; dropping the dial abandons it.
pub(super) async fn dial(origin: &Origin, verify: Verify, runtime: &Handle) -> Result<Dialed, Fault> {
    let origin = origin.clone();
    let mut dialing = Dialing(runtime.spawn(async move { connect(&origin, verify).await }));
    (&mut dialing.0).await.map_err(|error| Fault::Lost(error.to_string()))?
}

/// A connection and its request sender.
type Dialed = (Arc<Quic>, client::SendRequest);

/// A dial running on another runtime, aborted once nothing awaits it.
struct Dialing(JoinHandle<Result<Dialed, Fault>>);

impl Drop for Dialing {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Each address in turn, IPv4 first; the last address's fault if none connects.
async fn connect(origin: &Origin, verify: Verify) -> Result<Dialed, Fault> {
    let crypto =
        QuicClientConfig::try_from(client_config(verify, Some(Protocol::Http3)).await).map_err(io::Error::other);
    let mut config = noq::ClientConfig::new(Arc::new(crypto.map_err(ConnectError::Io)?));
    config.transport_config(Arc::new(client_transport()));
    let resolved = resolve(&origin.host, origin.port).await;
    let mut addresses = resolved.map_err(ConnectError::Unreachable)?;
    addresses.sort_by_key(|address| !address.ip().to_canonical().is_ipv4());
    let name = match &origin.host {
        Host::Name(name) => name.clone(),
        Host::Ip(ip) => ip.to_string(),
    };
    let mut last = ConnectError::Unreachable(io::Error::new(io::ErrorKind::NotFound, "no address resolved")).into();
    for address in addresses {
        match attempt(&config, address, &name).await {
            Ok(dialed) => return Ok(dialed),
            Err(fault) => last = fault,
        }
    }
    Err(last)
}

async fn attempt(config: &noq::ClientConfig, address: SocketAddr, name: &str) -> Result<Dialed, Fault> {
    let local = match address {
        SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let (socket, _) = bind_udp(local, 1).map_err(ConnectError::Io)?;
    let runtime = noq::default_runtime().ok_or_else(|| io::Error::other("no runtime for QUIC"));
    let endpoint = noq::Endpoint::new(noq::EndpointConfig::default(), None, socket, runtime.map_err(ConnectError::Io)?);
    let endpoint = endpoint.map_err(ConnectError::Io)?;
    let connecting = endpoint.connect_with(config.clone(), address, name);
    let mut connecting = connecting.map_err(|error| ConnectError::Io(io::Error::other(error)))?;
    let silent = || ConnectError::Unreachable(io::Error::new(io::ErrorKind::TimedOut, "no QUIC answer"));
    let answered = timeout(SILENT, connecting.handshake_data()).await;
    answered.map_err(|_| silent())?.map_err(refused)?;
    let finished = timeout(ANSWERING, connecting).await;
    let connection = finished
        .map_err(|_| Fault::TimedOut("QUIC handshake"))?
        .map_err(refused)?;
    let (mut driver, requests) = client::new(connection.clone());
    let driver = tokio::spawn(async move {
        let _ = driver.drive().await;
    });
    let quic = Quic {
        connection,
        _endpoint: endpoint,
        driver: driver.abort_handle(),
        home: Handle::current(),
    };
    Ok((Arc::new(quic), requests))
}

/// A handshake's failure, with the TLS error a certificate check raised.
fn refused(error: noq::ConnectionError) -> Fault {
    let tls = match &error {
        noq::ConnectionError::TransportError(transport) => transport.crypto.as_deref(),
        _ => None,
    };
    match tls.and_then(|tls| tls.downcast_ref::<rustls::Error>()) {
        Some(tls) => ConnectError::Tls(tls.clone()).into(),
        None => ConnectError::Io(io::Error::other(error)).into(),
    }
}

/// A WebTransport session with its connection; dropping it closes both.
pub struct Session {
    session: webtransport::Session,
    quic: Arc<Quic>,
    client: Client,
    origin: Origin,
}

impl Client {
    /// A session at `route` on a connection of its own, which `home` runs, else the next pinned runtime.
    pub async fn session(
        &self,
        home: Option<&Handle>,
        origin: &Origin,
        route: Route,
        query: Vec<(&'static str, String)>,
    ) -> Result<Session, Fault> {
        let home = home.cloned().unwrap_or_else(|| self.shared.runtimes.next());
        let request = Request { query, ..Request::new(Method::CONNECT, origin, route) };
        let head = self.head(&request)?;
        let open = async {
            let (quic, requests) = dial(origin, self.shared.verify, &home).await?;
            let connected = webtransport::Session::connect(&requests, head).await;
            match connected.map_err(http3_fault)? {
                Ok((session, _)) => Ok(Session { session, quic, client: self.clone(), origin: origin.clone() }),
                Err(refused) => Err(self
                    .refusal(&request, refused.status(), refused.headers())
                    .unwrap_or_else(|| Fault::Malformed(format!("WebTransport refused with {}", refused.status())))),
            }
        };
        let opened = timeout(CONTROL_TIMEOUT, open).await;
        opened.unwrap_or(Err(Fault::TimedOut("WebTransport session")))
    }
}

impl Session {
    /// Whether the session and its connection still carry streams.
    pub(super) fn usable(&self) -> bool {
        !self.quic.closed() && self.ended().is_none()
    }

    /// The fault the session ended with, once it ended.
    pub(super) fn ended(&self) -> Option<Fault> {
        Some(match self.session.closed().now_or_never()? {
            Ok((code, _)) => match LaneEnding::from_webtransport_code(code) {
                Some(ending) => self.client.ending(&self.origin, ending),
                None => Fault::Lost(format!("WebTransport session closed with code {code}")),
            },
            Err(error) => http3_fault(error),
        })
    }

    /// What `error` on one of the session's streams means: the session's ending once it ended.
    pub fn fault(&self, error: http3::Error) -> Fault {
        self.ended().unwrap_or_else(|| http3_fault(error))
    }

    fn gone(&self) -> Fault {
        self.ended()
            .unwrap_or_else(|| Fault::Lost("WebTransport session ended".into()))
    }

    /// The next stream the server opened.
    pub async fn accept_uni(&self) -> Result<RecvStream, Fault> {
        self.session.accept_uni().await.ok_or_else(|| self.gone())
    }

    pub(super) async fn open_uni(&self) -> Result<SendStream, Fault> {
        self.session.open_uni().await.map_err(|error| self.fault(error))
    }

    pub(super) async fn send_datagram(&self, payload: &[u8]) -> Result<(), Fault> {
        let sent = self.session.send_datagram_wait(payload).await;
        sent.map_err(|error| self.fault(error))
    }

    pub(super) async fn read_datagram(&self) -> Result<Bytes, Fault> {
        self.session.read_datagram().await.ok_or_else(|| self.gone())
    }
}
