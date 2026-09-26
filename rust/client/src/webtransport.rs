//! One explicitly owned WebTransport session per QUIC connection.
use crate::Error;
use crate::net::Http;
use bytes::{Buf, Bytes};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::capsule;
use h3::{ConnectionState, quic::RecvStream as _, stream::BufRecvStream};
use h3_noq::webtransport_send::{PendingReset, ResetQueue};
use http::Request;
use std::{
    collections::HashMap,
    future::{Future, poll_fn},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::JoinSet,
    time::timeout,
};

pub use h3_noq::webtransport_send::SendStream;
type RawReceive = BufRecvStream<h3_noq::RecvStream, Bytes>;
const CANCEL: u64 = 0x52e4a40fa8db;
const QUEUE: usize = 32;
const DATAGRAM_BYTES: usize = 256 * 1024;
// Bounded outstanding data per session. Four MiB constrained one upload
// stream on a simulated 100 ms RTT path after the receiver windows grew.
const SEND_WINDOW_BYTES: u64 = 8 * 1024 * 1024;

struct Endpoint(quinn::Endpoint);
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.0.close(0_u32.into(), b"WebTransport owner dropped");
    }
}

/// Queues are bounded. A full stream queue closes the connection; datagrams may
/// be dropped under load, matching their unreliable transport semantics.
pub struct Session {
    endpoint: Endpoint,
    connection: quinn::Connection,
    id: u64,
    sender: Option<h3::client::SendRequest<h3_noq::OpenStreams, Bytes>>,
    driver: JoinSet<()>,
    resets: ResetQueue,
    streams: Mutex<mpsc::Receiver<ReceiveStream>>,
    datagrams: Mutex<mpsc::Receiver<Datagram>>,
    graceful_connect_close: Arc<AtomicBool>,
}
struct Datagram {
    bytes: Bytes,
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug)]
pub struct ConnectRejected {
    pub status: http::StatusCode,
    pub headers: http::HeaderMap,
}
impl std::fmt::Display for ConnectRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WebTransport CONNECT refused: {}", self.status)
    }
}
impl std::error::Error for ConnectRejected {}

impl Session {
    /// Apply the shared pinned-origin grant policy and authentication refusal handling.
    pub async fn dial(
        http: &crate::net::Http,
        target: &str,
        insecure: bool,
        deadline: Duration,
    ) -> Result<Self, Error> {
        let mut request = Request::get(target).body(())?;
        if let Some(auth) = http.authorization(target) {
            if insecure {
                return Err("authenticated operation refuses insecure TLS".into());
            }
            request
                .headers_mut()
                .insert(http::header::AUTHORIZATION, auth);
        }
        match Self::connect(request, insecure, deadline).await {
            Err(error) => {
                if let Some(rejected) = error.downcast_ref::<ConnectRejected>() {
                    http.check_status(target, rejected.status, &rejected.headers)?;
                }
                Err(error)
            }
            result => result,
        }
    }
    /// Request must be an absolute HTTPS URL. Authorization headers are sent only
    /// to that URL; redirects are never followed. One session owns one connection.
    pub async fn connect(
        mut request: Request<()>,
        insecure: bool,
        deadline: Duration,
    ) -> Result<Self, Error> {
        timeout(deadline, async {
            let uri = request.uri();
            if uri.scheme_str() != Some("https")
                || uri.authority().is_none_or(|a| a.as_str().contains('@'))
            {
                return Err("WebTransport requires an HTTPS URL without userinfo".into());
            }
            let host = uri
                .host()
                .ok_or("WebTransport URL has no host")?
                .trim_start_matches('[')
                .trim_end_matches(']');
            let authority = uri.authority().unwrap();
            let port = if authority.as_str().len() == authority.host().len() {
                443
            } else {
                uri.port_u16().ok_or("invalid WebTransport port")?
            };
            let addresses: Vec<_> = tokio::net::lookup_host((host, port)).await?.collect();
            let tls = crate::tls::config(insecure)?;
            let mut config = quinn::ClientConfig::new(Arc::new(
                quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
            ));
            let mut transport = quinn::TransportConfig::default();
            transport.max_concurrent_bidi_streams(0_u32.into());
            transport.max_concurrent_uni_streams(36_u32.into());
            crate::quic_config::set_receive_credit(&mut transport);
            transport.send_window(SEND_WINDOW_BYTES);
            transport.datagram_receive_buffer_size(Some(DATAGRAM_BYTES));
            transport.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
            config.transport_config(Arc::new(transport));
            let mut connected = None;
            let mut last_error: Option<Error> = None;
            for address in addresses {
                let endpoint = Endpoint(quinn::Endpoint::new(
                    quinn::EndpointConfig::default(),
                    None,
                    graphite_meter_core::socket::udp_socket(
                        if address.is_ipv6() {
                            "[::]:0"
                        } else {
                            "0.0.0.0:0"
                        }
                        .parse()?,
                    )?,
                    quinn::default_runtime().ok_or("no async runtime for QUIC")?,
                )?);
                match timeout(
                    Duration::from_secs(3),
                    endpoint.0.connect_with(config.clone(), address, host)?,
                )
                .await
                {
                    Ok(Ok(connection)) => {
                        connected = Some((endpoint, connection));
                        break;
                    }
                    Ok(Err(error)) => last_error = Some(error.into()),
                    Err(error) => last_error = Some(error.into()),
                }
            }
            let (endpoint, connection) = connected.ok_or_else(|| {
                last_error.unwrap_or_else(|| "WebTransport hostname has no addresses".into())
            })?;
            let (resets, reset_rx) = ResetQueue::new(QUEUE);
            let (stream_tx, stream_rx) = mpsc::channel(QUEUE);
            let (datagram_tx, datagram_rx) = mpsc::channel(QUEUE);
            let graceful_connect_close = Arc::new(AtomicBool::new(false));
            let mut owner = Self {
                endpoint,
                connection,
                id: 0,
                sender: None,
                driver: JoinSet::new(),
                resets,
                streams: Mutex::new(stream_rx),
                datagrams: Mutex::new(datagram_rx),
                graceful_connect_close: graceful_connect_close.clone(),
            };
            let (http, mut sender) = h3::client::builder()
                .max_field_section_size(32 * 1024)
                .enable_extended_connect(true)
                .enable_datagram(true)
                .enable_webtransport(true)
                .max_webtransport_sessions(1)
                .build(h3_noq::Connection::new(owner.connection.clone()))
                .await?;
            let (ready_tx, ready_rx) = oneshot::channel();
            let (id_tx, id_rx) = oneshot::channel();
            let quic = owner.connection.clone();
            owner.driver.spawn(async move {
                let result = drive(
                    http,
                    quic.clone(),
                    ready_tx,
                    id_rx,
                    stream_tx,
                    datagram_tx,
                    reset_rx,
                )
                .await;
                quic.close(
                    0_u32.into(),
                    if result.is_ok() {
                        b"session complete"
                    } else {
                        b"session failed"
                    },
                );
            });
            ready_rx
                .await
                .map_err(|_| "peer did not negotiate WebTransport")?;
            *request.method_mut() = http::Method::CONNECT;
            request
                .extensions_mut()
                .insert(h3::ext::Protocol::WEB_TRANSPORT);
            request.headers_mut().insert(
                "sec-webtransport-http3-draft02",
                http::HeaderValue::from_static("1"),
            );
            let mut stream = sender.send_request(request).await?;
            owner.id = stream.id().into_inner();
            id_tx
                .send(owner.id)
                .map_err(|_| "WebTransport driver stopped")?;
            owner.sender = Some(sender);
            let response = stream.recv_response().await?;
            if !response.status().is_success() {
                return Err(Box::new(ConnectRejected {
                    status: response.status(),
                    headers: response.headers().clone(),
                }) as Error);
            }
            let quic = owner.connection.clone();
            owner.driver.spawn(async move {
                let mut decoder = capsule::Decoder::new();
                let result: Result<(), Error> = async {
                    while let Some(mut data) = stream.recv_data().await? {
                        let data = data.copy_to_bytes(data.remaining());
                        if let Some(capsule) = decoder.feed(&data)?.into_iter().next() {
                            match capsule {
                                capsule::Capsule::CloseSession { .. } => return Ok(()),
                                _ => {
                                    return Err(
                                        "unnegotiated WebTransport flow control capsule".into()
                                    );
                                }
                            }
                        }
                    }
                    Ok(())
                }
                .await;
                graceful_connect_close.store(result.is_ok(), Ordering::Release);
                quic.close(
                    0_u32.into(),
                    if result.is_ok() {
                        b"CONNECT closed"
                    } else {
                        b"invalid CONNECT capsule"
                    },
                );
            });
            Ok(owner)
        })
        .await?
    }

    /// Send a Graphite ping; reply IDs and deadlines remain under caller control.
    pub async fn send_ping(&self, id: u32) -> Result<(), Error> {
        self.send_datagram(graphite_meter_core::wire::encode_ping(id).as_bytes())
            .await
    }
    pub async fn recv_pong(&self) -> Result<graphite_meter_core::wire::Pong, Error> {
        loop {
            let data = self.recv_datagram().await?;
            if let Ok(text) = std::str::from_utf8(&data)
                && let Ok(pong) = graphite_meter_core::wire::decode_pong(text)
            {
                return Ok(pong);
            }
        }
    }
    /// The upload route's first server stream carries newline-delimited progress.
    pub async fn upload_progress(&self) -> Result<UploadProgress, Error> {
        Ok(UploadProgress {
            stream: self.accept_uni().await?,
            buffered: Vec::new(),
            pending: Bytes::new(),
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }
    pub fn retryable_failure(&self, error: &Error) -> bool {
        match self.connection.close_reason() {
            Some(reason) => {
                self.graceful_connect_close.load(Ordering::Acquire) || retryable_quic_close(&reason)
            }
            None => retryable_stream_error(error),
        }
    }
    pub fn max_datagram_size(&self) -> Option<usize> {
        let mut prefix = Vec::new();
        capsule::encode_varint(self.id / 4, &mut prefix).ok()?;
        self.connection
            .max_datagram_size()?
            .checked_sub(prefix.len())
    }
    pub async fn send_datagram(&self, payload: &[u8]) -> Result<(), Error> {
        let max = self
            .max_datagram_size()
            .ok_or(quinn::SendDatagramError::UnsupportedByPeer)?;
        if payload.len() > max {
            return Err(quinn::SendDatagramError::TooLarge.into());
        }
        let mut frame = Vec::with_capacity(payload.len() + 8);
        capsule::encode_varint(self.id / 4, &mut frame)?;
        frame.extend_from_slice(payload);
        self.connection.send_datagram_wait(frame.into()).await?;
        Ok(())
    }
    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        Ok(self
            .datagrams
            .lock()
            .await
            .recv()
            .await
            .ok_or("WebTransport session closed")?
            .bytes)
    }
    pub async fn accept_uni(&self) -> Result<ReceiveStream, Error> {
        self.streams
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| "WebTransport session closed".into())
    }
    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        self.resets
            .open(&self.connection, self.id, quinn::VarInt::from_u64(CANCEL)?)
            .await
    }
    pub async fn close(mut self) {
        self.connection.close(0_u32.into(), b"session complete");
        self.driver.shutdown().await;
        let _ = timeout(Duration::from_secs(1), self.endpoint.0.wait_idle()).await;
    }
}

/// A stage group shares one live session. Replacement is serialized, so a
/// connection loss cannot make every lane dial its own replacement session.
pub struct SessionSlot {
    current: Mutex<Arc<Session>>,
    http: Http,
    target: String,
    insecure: bool,
}

impl SessionSlot {
    pub async fn dial(http: &Http, target: String, insecure: bool) -> Result<Self, Error> {
        let session = Session::dial(http, &target, insecure, Duration::from_secs(10)).await?;
        Ok(Self {
            current: Mutex::new(Arc::new(session)),
            http: http.clone(),
            target,
            insecure,
        })
    }

    pub async fn current(&self) -> Arc<Session> {
        self.current.lock().await.clone()
    }

    pub async fn reconnect(&self, failed: &Arc<Session>) -> Result<Arc<Session>, Error> {
        let mut current = self.current.lock().await;
        if !Arc::ptr_eq(&current, failed) {
            return Ok(current.clone());
        }
        if !failed.is_closed() {
            return Err("WebTransport stream failed while its session remained open".into());
        }
        let session = Session::dial(
            &self.http,
            &self.target,
            self.insecure,
            Duration::from_secs(10),
        )
        .await?;
        *current = Arc::new(session);
        Ok(current.clone())
    }

    pub async fn close(self) {
        let session = self.current.into_inner();
        if let Ok(session) = Arc::try_unwrap(session) {
            session.close().await;
        }
    }
}

/// Transfer lanes may resume after a lost connection or the application's
/// explicit cancellation reset. Other stream errors remain protocol failures.
pub fn retryable_stream_error(error: &Error) -> bool {
    const RESET: u64 = 0x52e4a40fa8db;
    if let Some(error) = error.downcast_ref::<h3::quic::StreamErrorIncoming>() {
        return match error {
            h3::quic::StreamErrorIncoming::ConnectionErrorIncoming { connection_error } => {
                retryable_h3_close(connection_error)
            }
            h3::quic::StreamErrorIncoming::StreamTerminated { error_code } => *error_code == RESET,
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<quinn::WriteError>() {
        return match error {
            quinn::WriteError::ConnectionLost(error) => retryable_quic_close(error),
            quinn::WriteError::Stopped(code) => code.into_inner() == RESET,
            _ => false,
        };
    }
    if let Some(quinn::SendDatagramError::ConnectionLost(error)) =
        error.downcast_ref::<quinn::SendDatagramError>()
    {
        return retryable_quic_close(error);
    }
    error
        .downcast_ref::<quinn::ConnectionError>()
        .is_some_and(retryable_quic_close)
}

fn retryable_h3_close(error: &h3::quic::ConnectionErrorIncoming) -> bool {
    match error {
        h3::quic::ConnectionErrorIncoming::Timeout => true,
        h3::quic::ConnectionErrorIncoming::ApplicationClose { error_code } => {
            *error_code == 0 || *error_code == h3::error::Code::H3_NO_ERROR.value()
        }
        h3::quic::ConnectionErrorIncoming::Undefined(error) => error
            .as_ref()
            .downcast_ref::<quinn::ConnectionError>()
            .is_some_and(retryable_quic_close),
        _ => false,
    }
}

fn retryable_quic_close(error: &quinn::ConnectionError) -> bool {
    match error {
        quinn::ConnectionError::Reset | quinn::ConnectionError::TimedOut => true,
        quinn::ConnectionError::ApplicationClosed(close) => {
            close.error_code.into_inner() == 0
                || close.error_code.into_inner() == h3::error::Code::H3_NO_ERROR.value()
        }
        _ => false,
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.connection.close(0_u32.into(), b"session dropped");
        self.driver.abort_all();
    }
}

/// Ordered, zero-copy chunks. Drop sends STOP_SENDING for unfinished payloads.
pub struct ReceiveStream {
    stream: RawReceive,
    finished: bool,
}
impl ReceiveStream {
    pub async fn read_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let data = poll_fn(|cx| self.stream.poll_data(cx))
            .await
            .map_err(|error| -> Error { Box::new(error) })?;
        if data.is_none() {
            self.finished = true;
        }
        Ok(data)
    }
}
impl Drop for ReceiveStream {
    fn drop(&mut self) {
        if !self.finished {
            self.stream.stop_sending(CANCEL);
        }
    }
}

type PendingHeaders = HashMap<u64, Pin<Box<tokio::time::Sleep>>>;
fn poll_incoming(
    http: &mut h3::client::Connection<h3_noq::Connection, Bytes>,
    ready: &mut Option<oneshot::Sender<()>>,
    pending: &mut PendingHeaders,
    id: Option<u64>,
    cx: &mut std::task::Context<'_>,
) -> Poll<Result<Option<(u64, RawReceive)>, Error>> {
    if let Poll::Ready(error) = http.poll_close(cx) {
        return Poll::Ready(Err::<Option<(u64, RawReceive)>, Error>(error.into()));
    }
    let settings = http.settings();
    if settings.enable_webtransport()
        && settings.max_webtransport_sessions() > 0
        && settings.enable_datagram()
        && settings.enable_extended_connect()
        && let Some(ready) = ready.take()
    {
        let _ = ready.send(());
    }
    let ids: Vec<_> = http
        .inner
        .pending_recv_stream_ids()
        .map(|id| id.into_inner())
        .collect();
    pending.retain(|id, _| ids.contains(id));
    for id in ids {
        pending
            .entry(id)
            .or_insert_with(|| Box::pin(tokio::time::sleep(Duration::from_secs(10))));
    }
    for timer in pending.values_mut() {
        if timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err("WebTransport stream header timeout".into()));
        }
    }
    if id.is_some()
        && let Some((session, stream)) = http.inner.accepted_streams_mut().wt_uni_streams.pop()
    {
        return Poll::Ready(Ok(Some((
            h3::quic::StreamId::from(session).into_inner(),
            stream,
        ))));
    }
    Poll::Pending
}

type Cleanup = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;
async fn drive(
    mut http: h3::client::Connection<h3_noq::Connection, Bytes>,
    quic: quinn::Connection,
    ready: oneshot::Sender<()>,
    mut session: oneshot::Receiver<u64>,
    streams: mpsc::Sender<ReceiveStream>,
    datagrams: mpsc::Sender<Datagram>,
    mut resets: mpsc::Receiver<PendingReset>,
) -> Result<(), Error> {
    let mut ready = Some(ready);
    let mut id = None;
    let mut pending = HashMap::new();
    let budget = Arc::new(Semaphore::new(DATAGRAM_BYTES));
    let mut cleanups: FuturesUnordered<Cleanup> = FuturesUnordered::new();
    // Declared after cleanup futures, so every return closes before dropping them.
    struct CloseFirst(quinn::Connection);
    impl Drop for CloseFirst {
        fn drop(&mut self) {
            self.0.close(0_u32.into(), b"WebTransport driver ended");
        }
    }
    let _close_first = CloseFirst(quic.clone());
    loop {
        tokio::select! {
            result = poll_fn(|cx| poll_incoming(&mut http, &mut ready, &mut pending, id, cx)) => {
                if let Some((stream_id, stream)) = result? {
                    let stream = ReceiveStream {stream, finished: false};
                    if Some(stream_id) != id {return Err("WebTransport stream has foreign session ID".into());}
                    streams.try_send(stream).map_err(|_| "WebTransport incoming stream queue full")?;
                }
            },
            session_id = &mut session, if id.is_none() => {id = Some(session_id?);},
            datagram = quic.read_datagram() => {
                let mut bytes = datagram?;
                let Some((quarter, length)) = capsule::decode_varint(&bytes)? else {return Err("truncated HTTP datagram".into());};
                if quarter >= (1 << 60) || id.is_some_and(|id| id / 4 != quarter) {return Err("foreign WebTransport datagram session".into());}
                // A datagram can arrive before the CONNECT task hands over its ID.
                if id.is_none() {continue;}
                let _ = bytes.split_to(length);
                if let Ok(permit) = budget.clone().try_acquire_many_owned(bytes.len() as u32) {
                    let _ = datagrams.try_send(Datagram {bytes, _permit: permit});
                }
            },
            reset = resets.recv() => {
                let Some(reset) = reset else {return Ok(());};
                let connection = quic.clone();
                cleanups.push(Box::pin(async move {
                    let completion = reset.complete();
                    tokio::pin!(completion);
                    tokio::select! {biased;
                        result = &mut completion => result,
                        _ = tokio::time::sleep(Duration::from_secs(10)) => {
                            connection.close(0_u32.into(), b"reset cleanup timeout");
                            Err("WebTransport reset cleanup timeout".into())
                        }
                    }
                }));
            },
            result = cleanups.next(), if !cleanups.is_empty() => {result.unwrap()?;},
        }
    }
}

/// Bounded incremental progress decoder; only server-observed counters are returned.
pub struct UploadProgress {
    stream: ReceiveStream,
    buffered: Vec<u8>,
    pending: Bytes,
}
impl UploadProgress {
    pub async fn next(&mut self) -> Result<graphite_meter_core::wire::UploadProgress, Error> {
        const MAX_LINE: usize = 16 * 1024;
        loop {
            if self.pending.is_empty() {
                self.pending = self
                    .stream
                    .read_chunk()
                    .await?
                    .ok_or("upload progress stream closed")?;
            }
            let end = self.pending.iter().position(|&byte| byte == b'\n');
            let count = end.map_or(self.pending.len(), |end| end + 1);
            if self.buffered.len() + count > MAX_LINE {
                return Err("upload progress line exceeds limit".into());
            }
            self.buffered
                .extend_from_slice(&self.pending.split_to(count));
            if end.is_some() {
                let line = std::mem::take(&mut self.buffered);
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return Ok(graphite_meter_core::wire::decode_upload_progress(&line)?);
            }
        }
    }
}

/// Uses the same loss accounting and reconnect boundaries as WebSocket latency.
pub async fn run_latency(
    http: &crate::net::Http,
    origin: &str,
    insecure: bool,
    interval: Duration,
    duration: Duration,
    observations: mpsc::Sender<crate::latency::Observation>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Error> {
    crate::latency::run_kind(
        http,
        origin,
        insecure,
        (interval, duration),
        observations,
        cancel,
        crate::latency::Kind::WebTransport,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_retry_excludes_protocol_and_local_transport_failures() {
        use h3::quic::{ConnectionErrorIncoming, StreamErrorIncoming};
        let retryable: Vec<Error> = vec![
            Box::new(StreamErrorIncoming::StreamTerminated { error_code: CANCEL }),
            Box::new(StreamErrorIncoming::ConnectionErrorIncoming {
                connection_error: ConnectionErrorIncoming::Timeout,
            }),
            Box::new(quinn::ConnectionError::Reset),
        ];
        for error in retryable {
            assert!(retryable_stream_error(&error), "{error}");
        }
        let fatal: Vec<Error> = vec![
            Box::new(StreamErrorIncoming::StreamTerminated { error_code: 42 }),
            Box::new(StreamErrorIncoming::ConnectionErrorIncoming {
                connection_error: ConnectionErrorIncoming::InternalError("adapter failure".into()),
            }),
            Box::new(quinn::ConnectionError::VersionMismatch),
            Box::new(quinn::ConnectionError::LocallyClosed),
        ];
        for error in fatal {
            assert!(!retryable_stream_error(&error), "{error}");
        }
    }
}
