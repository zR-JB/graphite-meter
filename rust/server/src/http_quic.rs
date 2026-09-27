//! A QUIC connection owns its request futures, session registry, and reset work.

use super::*;
use crate::{webtransport, webtransport_send::ResetQueue};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use quinn::SharedBudget;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use webtransport::{Incoming, ReceiveStream, TransportError};

const MAX_PENDING_STREAMS: usize = 64;
const SESSION_QUEUE: usize = 32;
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const WT_SESSION_GONE: u64 = 0x170d7b68;
const MIN_SEND_WINDOW: u64 = 2 * 1024 * 1024;
const MAX_SEND_WINDOW: u64 = 32 * 1024 * 1024;
const SEND_WINDOW_STEP: u64 = 256 * 1024;
const SEND_WINDOW_SHRINK_DELAY: Duration = Duration::from_secs(1);
const INCOMING_BYTES: u64 = 64 * 1024;
const INCOMING_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
const UNI_STREAMS: u32 = 23;
const RECEIVE_WINDOW: u32 = 16 * 1024 * 1024;
// One maximal 64 KiB HTTP/3 frame of credit until admitted work raises it.
const RECEIVE_WINDOW_FLOOR: u32 = 64 * 1024;
// h3 copies one maximal 64 KiB frame plus a 16 KiB block per stream outside Noq's pools.
const STREAM_FLOOR_BYTES: usize = 80 * 1024;
type Work = Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send>>;

impl HttpServer {
    pub(crate) fn quic_endpoint(
        &self,
        tls: Arc<rustls::ServerConfig>,
        address: SocketAddr,
    ) -> Result<quinn::Endpoint, ConfigError> {
        let socket = graphite_meter_core::socket::udp_socket(address)?;
        let socket_buffers = socket2::SockRef::from(&socket);
        let kernel_bytes = socket_buffers
            .recv_buffer_size()?
            .checked_add(socket_buffers.send_buffer_size()?)
            .ok_or("UDP socket buffer size overflow")?;
        let runtime = quinn::default_runtime().ok_or("no async runtime for QUIC")?;
        let socket = runtime.wrap_udp_socket(socket)?;
        let endpoint_config = quinn::EndpointConfig::default();
        let bytes = endpoint_bytes(
            &endpoint_config,
            self.config.max_connections,
            kernel_bytes,
            socket.max_receive_segments().get(),
        )
        .ok_or("QUIC endpoint buffer size overflow")?;
        check_buffer_budget(
            &self.config,
            self.memory.limit,
            self.handshake_bytes.load(Ordering::Relaxed),
            bytes,
        )?;
        let lease = Arc::new(
            self.memory
                .lease(bytes)
                .ok_or("server memory budget cannot cover QUIC endpoint buffers")?,
        );
        self.endpoint_bytes.store(bytes, Ordering::Relaxed);
        quinn::Endpoint::new_with_abstract_socket(
            endpoint_config,
            Some(self.quic_config(tls)?),
            Box::new(BudgetedSocket { socket, lease }),
            runtime,
        )
        .map_err(Into::into)
    }

    pub fn quic_config(&self, tls: Arc<rustls::ServerConfig>) -> Result<quinn::ServerConfig, ConfigError> {
        if tls.alpn_protocols != [b"h3".to_vec()] {
            return Err("HTTP/3 listener requires h3-only TLS ALPN".into());
        }
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        config
            .max_incoming(self.config.max_connections)
            .incoming_buffer_size(INCOMING_BYTES)
            .incoming_buffer_size_total(INCOMING_TOTAL_BYTES);
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(u32::try_from(max_requests(&self.config.limits))?.into());
        transport.max_concurrent_uni_streams(UNI_STREAMS.into());
        // A fixed 1 MiB Noq stream window capped one upload near 80 Mbit/s at 100 ms RTT.
        transport.stream_receive_window((8 * 1024 * 1024_u32).into());
        transport.receive_window(RECEIVE_WINDOW_FLOOR.into());
        transport.send_window(MIN_SEND_WINDOW);
        transport.shared_budget(Some(self.memory.clone()));
        transport.datagram_receive_buffer_size(Some(64 * 1024));
        transport.datagram_send_buffer_size(64 * 1024);
        transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
        config.transport_config(Arc::new(transport));
        Ok(config)
    }

    pub async fn serve_quic(
        self: Arc<Self>,
        endpoint: quinn::Endpoint,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ConfigError> {
        tokio::pin!(shutdown);
        let mut connections = JoinSet::new();
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                Some(_) = connections.join_next() => {}
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break Ok(()); };
                    // Retry spends a round trip to protect admission under load.
                    if !incoming.remote_address_validated()
                        && (self.connections.stats().active >= self.config.max_connections / 4
                            || self.memory.under_pressure())
                    {
                        let _ = incoming.retry();
                        continue;
                    }
                    let peer = incoming.remote_address();
                    let Ok(permit) = self.connections.acquire_buffered(peer, true) else {
                        incoming.refuse();
                        continue;
                    };
                    let floor = connection_floor(&self.config.limits, self.handshake_bytes.load(Ordering::Relaxed));
                    let Some(lease) = self.memory.lease(floor) else {
                        incoming.refuse();
                        continue;
                    };
                    let Ok(connecting) = incoming.accept() else { continue; };
                    let server = self.clone();
                    connections.spawn(async move {
                        let (_permit, _lease) = (permit, lease);
                        let quic = tokio::select! {
                            biased;
                            _ = stopped(server.stopping.clone()) => return,
                            result = tokio::time::timeout(Duration::from_secs(5), connecting) => match result {
                                Ok(Ok(quic)) => quic,
                                Ok(Err(error)) => {
                                    if !ended_normally(&error) {
                                        server.peers.write(format_args!("[gm:h3] QUIC handshake error from {}: {error}", peer.ip().to_canonical()));
                                    }
                                    return;
                                }
                                Err(_) => {
                                    server.peers.write(format_args!("[gm:h3] QUIC handshake error from {}: timed out", peer.ip().to_canonical()));
                                    return;
                                }
                            }
                        };
                        let stopping = server.stopping.clone();
                        if let Err(error) = server.clone().serve_quic_connection(quic, peer).await
                            && !*stopping.borrow()
                            && !ended_normally(&*error)
                        {
                            server.peers.write(format_args!("[gm:h3] webtransport connection: {:?}", error.to_string()));
                        }
                    });
                }
            }
        };
        self.stopping.send_replace(true);
        let drain_deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        let _ = tokio::time::timeout_at(drain_deadline, async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        endpoint.close(0_u32.into(), b"server stopped");
        connections.shutdown().await;
        let _ = tokio::time::timeout_at(drain_deadline, endpoint.wait_idle()).await;
        result
    }

    async fn serve_quic_connection(
        self: Arc<Self>,
        quic: quinn::Connection,
        peer: SocketAddr,
    ) -> Result<(), TransportError> {
        let max_requests = max_requests(&self.config.limits);
        let (resets, mut pending_resets) = ResetQueue::new(max_requests);
        let credit = self.receive_credit(quic.clone());
        let mut window = SendWindow::new();
        let mut initializing = CloseOnDrop(Some(quic.clone()));
        let http = tokio::time::timeout(HEADER_TIMEOUT, webtransport::Connection::new(quic.clone(), 1)).await??;
        let mut connection = OwnedConnection {
            http,
            quic,
            requests: FuturesUnordered::new(),
            cleanup: FuturesUnordered::new(),
            sessions: Sessions::default(),
            served_requests: false,
        };
        let active_responses = Arc::new(AtomicUsize::new(0));
        initializing.0.take();
        let stopping = stopped(self.stopping.clone());
        tokio::pin!(stopping);
        let mut closing = false;
        let close = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(close);
        let idle = tokio::time::sleep(IDLE_TIMEOUT);
        tokio::pin!(idle);
        let mut leftover = None;
        let stale = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(stale);
        let mut expiry = tokio::time::interval(Duration::from_secs(1));
        expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tuning = tokio::time::interval(Duration::from_millis(250));
        tuning.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let since = credit.leftover_since();
            if since != leftover {
                leftover = since;
                if let Some(since) = since {
                    stale.as_mut().reset(since + IDLE_TIMEOUT);
                }
            }
            tokio::select! {
                _ = &mut stale, if leftover.is_some() && !closing => {
                    closing = true;
                    connection.goaway();
                    close.as_mut().reset(tokio::time::Instant::now() + SHUTDOWN_GRACE);
                    if connection.finished(closing) { return Ok(()); }
                }
                _ = &mut stopping, if !closing => {
                    closing = true;
                    connection.goaway();
                    close.as_mut().reset(tokio::time::Instant::now() + SHUTDOWN_GRACE);
                    if connection.finished(closing) { return Ok(()); }
                }
                _ = &mut close, if closing => return Ok(()),
                _ = expiry.tick() => connection.sessions.expire(),
                _ = tuning.tick() => {
                    if connection.requests.is_empty() {
                        window.release(&connection.quic);
                    } else {
                        window.update(&connection.quic, &self.memory);
                    }
                }
                Some(plain) = connection.requests.next() => {
                    connection.served_requests |= plain;
                    if connection.finished(closing) { return Ok(()); }
                    if connection.requests.is_empty() {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                    }
                },
                _ = &mut idle, if connection.requests.is_empty() => {
                    connection.goaway();
                    return Ok(());
                },
                Some(_) = connection.cleanup.next() => {
                    if connection.finished(closing) { return Ok(()); }
                },
                Some(reset) = pending_resets.recv() => {
                    let quic = connection.quic.clone();
                    connection.cleanup.push(Box::pin(async move {
                        let completion = reset.complete();
                        tokio::pin!(completion);
                        tokio::select! {
                            result = &mut completion => result,
                            _ = tokio::time::sleep(HEADER_TIMEOUT) => {
                                // Close while the cleanup future still owns the
                                // prefix; only then may that future be dropped.
                                quic.close(0_u32.into(), b"reliable prefix cleanup timed out");
                                Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into())
                            }
                        }
                    }));
                }
                incoming = connection.http.next() => {
                    let Some(incoming) = incoming? else { return Ok(()); };
                    match incoming {
                        Incoming::Request(request) => {
                            if connection.requests.len() >= max_requests {
                                drop(request);
                                continue;
                            }
                            let server = self.clone();
                            let sessions = connection.sessions.clone();
                            let resets = resets.clone();
                            let credit = credit.clone();
                            let active_responses = active_responses.clone();
                            connection.requests.push(Box::pin(async move {
                                let Ok(Ok((request, stream))) = tokio::time::timeout(HEADER_TIMEOUT, request.resolve_request()).await else { return false };
                                if request.method() == Method::CONNECT {
                                    let _ = server.serve_webtransport(request, stream, credit, peer, resets, sessions).await;
                                    false
                                } else {
                                    let _ = server.serve_http3_request(request, stream, peer, credit, active_responses).await;
                                    true
                                }
                            }));
                        }
                        Incoming::Datagram { session_id, payload } => connection.sessions.datagram(session_id, payload),
                        Incoming::Unidirectional { session_id, stream } => connection.sessions.stream(session_id, stream),
                    }
                }
            }
        }
    }
}

fn ended_normally(error: &(dyn std::error::Error + 'static)) -> bool {
    use h3::{
        error::{ConnectionError as Http3, LocalError},
        quic::ConnectionErrorIncoming as Quic,
    };
    let quic = |error: &quinn::ConnectionError| match error {
        quinn::ConnectionError::ApplicationClosed(_)
        | quinn::ConnectionError::TimedOut
        | quinn::ConnectionError::LocallyClosed => true,
        quinn::ConnectionError::ConnectionClosed(close) => close.error_code == quinn::TransportErrorCode::NO_ERROR,
        _ => false,
    };
    match error.downcast_ref::<Http3>() {
        Some(Http3::Remote(Quic::ApplicationClose { .. } | Quic::Timeout) | Http3::Timeout) => true,
        Some(Http3::Remote(Quic::Undefined(error))) => error.downcast_ref().is_some_and(quic),
        Some(error @ Http3::Local { error: local }) => {
            error.is_h3_no_error() || matches!(local, LocalError::Closing { .. })
        }
        Some(_) => false,
        None => error.downcast_ref().is_some_and(quic),
    }
}

#[derive(Debug)]
struct BudgetedSocket {
    socket: Box<dyn quinn::AsyncUdpSocket>,
    lease: Arc<Lease>,
}

impl quinn::AsyncUdpSocket for BudgetedSocket {
    fn create_sender(&self) -> Pin<Box<dyn quinn::UdpSender>> {
        Box::pin(BudgetedSender {
            sender: self.socket.create_sender(),
            _lease: self.lease.clone(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [quinn::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.socket.poll_recv(cx, bufs, meta)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn max_receive_segments(&self) -> std::num::NonZeroUsize {
        self.socket.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.socket.may_fragment()
    }
}

#[derive(Debug)]
struct BudgetedSender {
    sender: Pin<Box<dyn quinn::UdpSender>>,
    _lease: Arc<Lease>,
}

impl quinn::UdpSender for BudgetedSender {
    fn poll_send(
        mut self: Pin<&mut Self>,
        transmit: &quinn::udp::Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        self.sender.as_mut().poll_send(transmit, cx)
    }

    fn max_transmit_segments(&self) -> std::num::NonZeroUsize {
        self.sender.max_transmit_segments()
    }
}

pub(super) fn max_requests(limits: &crate::admission::Limits) -> usize {
    limits.operations_per_client + limits.sessions_per_client + 4
}

pub(super) fn connection_floor(limits: &crate::admission::Limits, handshake_bytes: usize) -> usize {
    (max_requests(limits) + UNI_STREAMS as usize)
        .saturating_mul(STREAM_FLOOR_BYTES)
        .saturating_add(handshake_bytes)
}

pub(super) fn endpoint_bytes(
    config: &quinn::EndpointConfig,
    max_connections: usize,
    kernel_bytes: usize,
    receive_segments: usize,
) -> Option<usize> {
    let packet = usize::try_from(config.get_max_udp_payload_size().min(64 * 1024)).ok()?;
    let receive = packet.checked_mul(receive_segments)?;
    receive
        .checked_mul(quinn::udp::BATCH_SIZE)?
        .checked_add(receive.checked_mul(max_connections.checked_add(1)?)?)?
        .checked_add(INCOMING_TOTAL_BYTES as usize)?
        .checked_add(kernel_bytes)
}

#[derive(Debug)]
pub(super) struct MemoryBudget {
    pub(super) limit: usize,
    used: AtomicUsize,
}

impl MemoryBudget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
        })
    }

    pub(super) fn lease(self: &Arc<Self>, bytes: usize) -> Option<Lease> {
        self.try_charge(bytes).then(|| Lease {
            budget: self.clone(),
            bytes,
        })
    }

    #[cfg(test)]
    pub(super) fn available(&self) -> usize {
        self.limit - self.used.load(Ordering::Relaxed)
    }

    fn under_pressure(&self) -> bool {
        self.used.load(Ordering::Relaxed) >= self.limit / 4
    }

    pub(super) fn has_headroom(&self) -> bool {
        self.used.load(Ordering::Relaxed) < self.limit / 4 * 3
    }
}

impl SharedBudget for MemoryBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|&used| used <= self.limit)
            })
            .is_ok()
    }

    fn refund(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub(super) struct Lease {
    budget: Arc<MemoryBudget>,
    bytes: usize,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.refund(self.bytes);
    }
}

#[derive(Clone)]
pub struct ReceiveCredit(Arc<CreditState>);

struct CreditState {
    quic: quinn::Connection,
    memory: Arc<MemoryBudget>,
    admitted: Mutex<Admission>,
}

#[derive(Default)]
struct Admission {
    active: usize,
    granted: bool,
    ended: Option<tokio::time::Instant>,
}

impl HttpServer {
    pub fn receive_credit(&self, quic: quinn::Connection) -> ReceiveCredit {
        ReceiveCredit(Arc::new(CreditState {
            quic,
            memory: self.memory.clone(),
            admitted: Mutex::default(),
        }))
    }
}

impl ReceiveCredit {
    pub(super) fn quic(&self) -> &quinn::Connection {
        &self.0.quic
    }

    pub(super) fn admit(&self) -> Admitted {
        let mut admitted = self.0.admitted.lock().expect("receive credit poisoned");
        admitted.ended = None;
        if admitted.active == 0 && self.0.memory.has_headroom() {
            self.0.quic.set_receive_window(RECEIVE_WINDOW.into());
            admitted.granted = true;
        }
        admitted.active += 1;
        Admitted(self.clone())
    }

    fn leftover_since(&self) -> Option<tokio::time::Instant> {
        self.0.admitted.lock().expect("receive credit poisoned").ended
    }
}

pub(super) struct Admitted(ReceiveCredit);

impl Drop for Admitted {
    fn drop(&mut self) {
        let credit = &self.0.0;
        let mut admitted = credit.admitted.lock().expect("receive credit poisoned");
        admitted.active -= 1;
        if admitted.active == 0 {
            credit.quic.set_receive_window(RECEIVE_WINDOW_FLOOR.into());
            if std::mem::take(&mut admitted.granted) {
                admitted.ended = Some(tokio::time::Instant::now());
            }
        }
    }
}

struct SendWindow {
    limit: u64,
    last: Option<(tokio::time::Instant, u64)>,
    low_demand_since: Option<tokio::time::Instant>,
}

impl SendWindow {
    fn new() -> Self {
        Self {
            limit: MIN_SEND_WINDOW,
            last: None,
            low_demand_since: None,
        }
    }

    fn update(&mut self, connection: &quinn::Connection, budget: &MemoryBudget) {
        // Read the aggregate first so a concurrent send on the initial path
        // cannot look like traffic on another path.
        let all_sent = connection.stats().udp_tx.bytes;
        let Some(path) = connection.path_stats(quinn::PathId::ZERO) else {
            self.last = None;
            self.low_demand_since = None;
            return;
        };
        // A second path invalidates this path's throughput estimate.
        if all_sent > path.udp_tx.bytes {
            self.last = None;
            self.low_demand_since = None;
            return;
        }
        let now = tokio::time::Instant::now();
        let sent = path.udp_tx.bytes;
        if let Some((last, previous)) = self.last {
            // Two observed bandwidth-delay products allow a new path to
            // grow without a full window on fast local links.
            let target = desired_send_window(sent.saturating_sub(previous), path.rtt, now.duration_since(last));
            if target == MIN_SEND_WINDOW && self.limit != MIN_SEND_WINDOW {
                let since = self.low_demand_since.get_or_insert(now);
                if now.duration_since(*since) >= SEND_WINDOW_SHRINK_DELAY {
                    self.release(connection);
                }
            } else {
                self.low_demand_since = None;
                if let Some(window) = self.grow(target, budget) {
                    connection.set_send_window(window);
                }
            }
        }
        self.last = Some((now, sent));
    }

    fn grow(&mut self, target: u64, budget: &MemoryBudget) -> Option<u64> {
        let granted = target.min(MAX_SEND_WINDOW).saturating_sub(self.limit);
        if granted < SEND_WINDOW_STEP || !budget.has_headroom() {
            return None;
        }
        self.limit += granted;
        Some(self.limit)
    }

    fn release(&mut self, connection: &quinn::Connection) {
        self.last = None;
        self.low_demand_since = None;
        if self.limit != MIN_SEND_WINDOW {
            self.limit = MIN_SEND_WINDOW;
            connection.set_send_window(MIN_SEND_WINDOW);
        }
    }
}

fn desired_send_window(sent: u64, rtt: Duration, elapsed: Duration) -> u64 {
    let Some(demand) = u128::from(sent)
        .saturating_mul(rtt.as_nanos())
        .saturating_mul(2)
        .checked_div(elapsed.as_nanos())
    else {
        return MIN_SEND_WINDOW;
    };
    demand.clamp(u128::from(MIN_SEND_WINDOW), u128::from(MAX_SEND_WINDOW)) as u64
}

struct OwnedConnection {
    http: webtransport::Connection,
    quic: quinn::Connection,
    requests: FuturesUnordered<Pin<Box<dyn Future<Output = bool> + Send>>>,
    cleanup: FuturesUnordered<Work>,
    sessions: Sessions,
    served_requests: bool,
}

impl OwnedConnection {
    // Browsers would hold a sessions-only connection's client slot ~15 s.
    fn finished(&self, closing: bool) -> bool {
        self.requests.is_empty()
            && self.cleanup.is_empty()
            && (closing || !self.served_requests && self.sessions.carried())
    }

    fn goaway(&mut self) {
        let _ = self.http.shutdown().now_or_never();
    }
}

struct CloseOnDrop(Option<quinn::Connection>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(quic) = &self.0 {
            quic.close(0_u32.into(), b"HTTP/3 initialization stopped");
        }
    }
}

impl Drop for OwnedConnection {
    fn drop(&mut self) {
        // End the reliable-prefix obligation before dropping cleanup futures,
        // including when the parent server task is cancelled or panics.
        self.quic.close(
            quinn::VarInt::from_u64(h3::error::Code::H3_NO_ERROR.value()).expect("H3 code"),
            b"connection ended",
        );
    }
}

pub(super) struct Datagram {
    pub(super) payload: Bytes,
    pub(super) _budget: tokio::sync::OwnedSemaphorePermit,
}

struct SessionSenders {
    streams: mpsc::Sender<ReceiveStream>,
    datagrams: mpsc::Sender<Datagram>,
}

pub(super) struct SessionReceivers {
    pub(super) streams: mpsc::Receiver<ReceiveStream>,
    pub(super) datagrams: mpsc::Receiver<Datagram>,
}

#[derive(Default)]
struct Registry {
    active: HashMap<u64, SessionSenders>,
    pending: VecDeque<(tokio::time::Instant, u64, ReceiveStream)>,
    carried: bool,
}

#[derive(Clone)]
pub(super) struct Sessions {
    registry: Arc<Mutex<Registry>>,
    datagram_bytes: Arc<tokio::sync::Semaphore>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            registry: Arc::default(),
            datagram_bytes: Arc::new(tokio::sync::Semaphore::new(256 * 1024)),
        }
    }
}

impl Sessions {
    pub(super) fn register(&self, id: u64) -> Option<(Registration, SessionReceivers)> {
        let (streams, stream_receiver) = mpsc::channel(SESSION_QUEUE);
        let (datagrams, datagram_receiver) = mpsc::channel(256);
        let mut registry = self.registry.lock().expect("session registry poisoned");
        // Multiple sessions require negotiated per-session flow control.
        // Reserve atomically after authorization but before sending success.
        if !registry.active.is_empty() {
            return None;
        }
        registry.carried = true;
        registry.active.insert(
            id,
            SessionSenders {
                streams: streams.clone(),
                datagrams,
            },
        );
        let mut remaining = VecDeque::new();
        while let Some((started, target, stream)) = registry.pending.pop_front() {
            if target == id {
                deliver(&streams, stream);
            } else {
                remaining.push_back((started, target, stream));
            }
        }
        registry.pending = remaining;
        Some((
            Registration {
                sessions: self.clone(),
                id,
            },
            SessionReceivers {
                streams: stream_receiver,
                datagrams: datagram_receiver,
            },
        ))
    }

    fn carried(&self) -> bool {
        self.registry.lock().expect("session registry poisoned").carried
    }

    fn datagram(&self, id: u64, payload: Bytes) {
        let Ok(size) = u32::try_from(payload.len() + 8 + size_of::<Datagram>()) else {
            return;
        };
        let Ok(budget) = self.datagram_bytes.clone().try_acquire_many_owned(size) else {
            return;
        };
        let registry = self.registry.lock().expect("session registry poisoned");
        if let Some(sender) = registry.active.get(&id) {
            // Noq owns exact wire bytes; retain the stripped session prefix charge.
            let _ = sender.datagrams.try_send(Datagram {
                payload,
                _budget: budget,
            });
        }
    }

    fn stream(&self, id: u64, stream: ReceiveStream) {
        let mut registry = self.registry.lock().expect("session registry poisoned");
        if let Some(sender) = registry.active.get(&id) {
            deliver(&sender.streams, stream);
        } else if registry.pending.len() < MAX_PENDING_STREAMS {
            registry.pending.push_back((tokio::time::Instant::now(), id, stream));
        } else {
            stop(stream);
        }
    }

    fn expire(&self) {
        let mut registry = self.registry.lock().expect("session registry poisoned");
        while registry
            .pending
            .front()
            .is_some_and(|(started, _, _)| started.elapsed() >= HEADER_TIMEOUT)
        {
            let (_, _, stream) = registry.pending.pop_front().expect("checked pending stream");
            stop(stream);
        }
    }
}

pub(super) struct Registration {
    sessions: Sessions,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.sessions
            .registry
            .lock()
            .expect("session registry poisoned")
            .active
            .remove(&self.id);
    }
}

fn deliver(sender: &mpsc::Sender<ReceiveStream>, stream: ReceiveStream) {
    if let Err(error) = sender.try_send(stream) {
        stop(error.into_inner());
    }
}

fn stop(mut stream: ReceiveStream) {
    use h3::quic::RecvStream;
    stream.stop_sending(WT_SESSION_GONE);
}

#[cfg(test)]
mod tests {
    use super::{MAX_SEND_WINDOW, MIN_SEND_WINDOW, SendWindow, Sessions, desired_send_window};
    use std::{sync::Arc, time::Duration};

    fn tls() -> (Arc<rustls::ServerConfig>, quinn::ClientConfig) {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
        let (certificate, key) = crate::test_identity::generate_identity("localhost").unwrap();
        let certificate = CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap();
        let provider = Arc::new(crate::crypto::provider());
        let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.clone()],
                PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap(),
            )
            .unwrap();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).unwrap();
        let mut client = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client.alpn_protocols = vec![b"h3".to_vec()];
        let client = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client).unwrap(),
        ));
        (Arc::new(tls), client)
    }

    fn held_udp_socket(address: std::net::SocketAddr) -> Option<std::path::PathBuf> {
        let port = format!(":{:04X}", address.port());
        let sockets: Vec<_> = std::fs::read_to_string("/proc/net/udp")
            .unwrap()
            .lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                fields[1].ends_with(&port).then(|| format!("socket:[{}]", fields[9]))
            })
            .collect();
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .filter_map(|fd| std::fs::read_link(fd.path()).ok())
            .find(|link| sockets.iter().any(|socket| link.as_os_str() == socket.as_str()))
    }

    async fn settled(memory: &super::MemoryBudget, peers: &[&quinn::Connection]) -> usize {
        let activity = || {
            let datagrams = peers.iter().map(|peer| {
                let stats = peer.stats();
                (stats.udp_tx.datagrams, stats.udp_rx.datagrams)
            });
            (memory.available(), datagrams.collect::<Vec<_>>())
        };
        let mut last = activity();
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }
            let now = activity();
            if now == last {
                return now.0;
            }
            last = now;
        }
    }

    async fn download(quic: quinn::Connection, bytes: u64) -> Result<usize, h3::error::StreamError> {
        use bytes::Buf;
        let (mut driver, mut h3) = h3::client::new(h3_noq::Connection::new(quic)).await.unwrap();
        let driving = tokio::spawn(async move { driver.wait_idle().await });
        let mut stream = h3
            .send_request(
                http::Request::get(format!("https://localhost/download?bytes={bytes}"))
                    .body(())
                    .unwrap(),
            )
            .await?;
        stream.finish().await?;
        assert_eq!(stream.recv_response().await?.status(), http::StatusCode::OK);
        let mut received = 0;
        while let Some(data) = stream.recv_data().await? {
            received += data.remaining();
        }
        driving.abort();
        Ok(received)
    }

    type Sender = h3::client::SendRequest<h3_noq::OpenStreams, bytes::Bytes>;

    fn serve(
        server: &Arc<super::HttpServer>,
        tls: Arc<rustls::ServerConfig>,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), super::ConfigError>>,
    ) {
        let endpoint =
            quinn::Endpoint::server(server.quic_config(tls).unwrap(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.clone().serve_quic(endpoint, async {
            let _ = stopped.await;
        }));
        (address, stop, serving)
    }

    async fn h3_client(
        client: &quinn::Endpoint,
        config: quinn::ClientConfig,
        address: std::net::SocketAddr,
    ) -> (quinn::Connection, Sender) {
        let quic = client
            .connect_with(config, address, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (mut driver, sender) = h3::client::new(h3_noq::Connection::new(quic.clone())).await.unwrap();
        tokio::spawn(async move { driver.wait_idle().await });
        (quic, sender)
    }

    async fn upload_id(sender: &mut Sender) -> String {
        use bytes::Buf;
        let request = http::Request::post("https://localhost/upload/session")
            .body(())
            .unwrap();
        let mut session = sender.send_request(request).await.unwrap();
        session.finish().await.unwrap();
        assert_eq!(session.recv_response().await.unwrap().status(), http::StatusCode::OK);
        let mut body = Vec::new();
        while let Some(mut data) = session.recv_data().await.unwrap() {
            body.extend_from_slice(&data.copy_to_bytes(data.remaining()));
        }
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["uploadId"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn endpoint_leaves_every_connection_floor_and_refunds_on_drop() {
        use super::*;
        let config = Arc::new(Config {
            max_connections: 4,
            max_connections_per_client: 4,
            ..Config::default()
        });
        let floors = 4 * connection_floor(&config.limits, 0);
        let (tls, _) = tls();
        let address = "127.0.0.1:0".parse().unwrap();
        let measured = HttpServer::with_memory(config.clone(), 1 << 30).unwrap();
        let idle = measured.memory.available();
        let endpoint = measured.quic_endpoint(tls.clone(), address).unwrap();
        let minimum = idle - measured.memory.available() + floors + DOWNLOAD_BLOCK_BYTES;
        drop(endpoint);
        let small = HttpServer::with_memory(config.clone(), minimum - 1).unwrap();
        let available = small.memory.available();
        let Err(error) = small.quic_endpoint(tls.clone(), address) else {
            panic!("endpoint started without room for every connection floor");
        };
        assert!(error.to_string().contains(&format!("at least {minimum}")), "{error}");
        assert_eq!(small.memory.available(), available);

        let server = HttpServer::with_memory(config, minimum).unwrap();
        let available = server.memory.available();
        let endpoint = server.quic_endpoint(tls.clone(), address).unwrap();
        assert_eq!(server.memory.available(), floors);
        let Err(error) = server.cover_handshake(1) else {
            panic!("certificate chain admitted without room in every connection floor");
        };
        assert!(
            error.to_string().contains(&format!("at least {}", minimum + 4)),
            "{error}"
        );
        let remaining = server.memory.lease(floors).unwrap();
        assert!(server.quic_endpoint(tls, address).is_err());
        assert_eq!(server.memory.available(), 0);
        drop(remaining);
        let address = endpoint.local_addr().unwrap();
        let socket = held_udp_socket(address).unwrap();
        drop(endpoint);
        tokio::time::timeout(Duration::from_secs(5), async {
            while server.memory.available() != available {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_ne!(held_udp_socket(address), Some(socket));
    }

    #[tokio::test]
    async fn retained_udp_sender_keeps_socket_budget_until_last_drop() {
        use super::*;
        use quinn::AsyncUdpSocket;
        let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let runtime = quinn::default_runtime().unwrap();
        let socket = runtime
            .wrap_udp_socket(graphite_meter_core::socket::udp_socket("127.0.0.1:0".parse().unwrap()).unwrap())
            .unwrap();
        let memory = MemoryBudget::new(64 * 1024);
        let socket = BudgetedSocket {
            socket,
            lease: Arc::new(memory.lease(64 * 1024).unwrap()),
        };
        let address = socket.local_addr().unwrap();
        let mut sender = socket.create_sender();
        drop(socket);
        assert_eq!(memory.available(), 0);
        let transmit = quinn::udp::Transmit {
            destination: receiver.local_addr().unwrap(),
            ecn: None,
            contents: b"retained",
            segment_size: None,
            src_ip: None,
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            std::future::poll_fn(|cx| sender.as_mut().poll_send(&transmit, cx))
                .await
                .unwrap();
            let mut payload = [0; 32];
            let (length, peer) = receiver.recv_from(&mut payload).await.unwrap();
            assert_eq!(&payload[..length], b"retained");
            assert_eq!(peer, address);
        })
        .await
        .unwrap();
        assert!(std::net::UdpSocket::bind(address).is_err());
        let socket = held_udp_socket(address).unwrap();
        drop(sender);
        assert_eq!(memory.available(), 64 * 1024);
        assert_ne!(held_udp_socket(address), Some(socket));
    }

    #[test]
    fn send_window_keeps_local_traffic_small_but_allows_high_rtt_paths_to_grow() {
        let elapsed = Duration::from_millis(250);
        let sent = 8 * 1024 * 1024;
        assert_eq!(
            desired_send_window(sent, Duration::from_millis(1), elapsed),
            MIN_SEND_WINDOW
        );
        assert_eq!(
            desired_send_window(sent, Duration::from_millis(100), elapsed),
            2 * sent * 100 / 250
        );
        assert_eq!(
            desired_send_window(sent, Duration::from_secs(1), elapsed),
            MAX_SEND_WINDOW
        );
        assert_eq!(
            desired_send_window(sent, Duration::from_millis(100), Duration::ZERO),
            MIN_SEND_WINDOW
        );
    }

    #[tokio::test]
    async fn idle_send_window_shrinks_while_control_stream_stays_open() {
        let (tls, client_config) = tls();
        let config = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap(),
        ));
        let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        tokio::time::timeout(Duration::from_secs(5), async {
            let (client, server) = tokio::join!(
                client.connect(server.local_addr().unwrap(), "localhost").unwrap(),
                async { server.accept().await.unwrap().await.unwrap() },
            );
            let client = client.unwrap();
            let (mut request, mut response) = client.open_bi().await.unwrap();
            request.write_all(b"ping").await.unwrap();
            let (mut replies, mut requests) = server.accept_bi().await.unwrap();
            let mut ping = [0; 4];
            requests.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");

            let budget = super::MemoryBudget::new(usize::MAX);
            let mut window = SendWindow::new();
            server.set_send_window(window.grow(MAX_SEND_WINDOW, &budget).unwrap());
            window.update(&server, &budget);
            for sample in 0..5 {
                replies.write_all(b"pong").await.unwrap();
                response.read_exact(&mut ping).await.unwrap();
                assert_eq!(&ping, b"pong");
                tokio::time::pause();
                tokio::time::advance(Duration::from_millis(250)).await;
                tokio::time::resume();
                window.update(&server, &budget);
                if sample < 3 {
                    assert_eq!(
                        window.limit, MAX_SEND_WINDOW,
                        "one quiet sample must not shrink an active window"
                    );
                }
            }
            assert_eq!(window.limit, MIN_SEND_WINDOW);
            request.write_all(b"live").await.unwrap();
            requests.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"live");
            client.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn receive_window_grows_only_for_admitted_work_with_headroom() {
        use super::*;
        use futures_util::FutureExt;
        let server = HttpServer::new(Arc::new(Config::default())).unwrap();
        let (tls, client_config) = tls();
        let endpoint =
            quinn::Endpoint::server(server.quic_config(tls).unwrap(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        tokio::time::timeout(Duration::from_secs(5), async {
            let connect = || async {
                let (quic, accepted) = tokio::join!(
                    client.connect(endpoint.local_addr().unwrap(), "localhost").unwrap(),
                    async { endpoint.accept().await.unwrap().await.unwrap() },
                );
                (quic.unwrap(), accepted)
            };
            let (quic, accepted) = connect().await;
            let (active, active_server) = connect().await;
            let admitted_active = server.receive_credit(active_server.clone()).admit();
            let idle = settled(&server.memory, &[&quic, &active]).await;
            let mut send = quic.open_uni().await.unwrap();
            let mut written = 0;
            while let Some(Ok(count)) = send.write(&[7; 16 * 1024]).now_or_never() {
                written += count;
            }
            assert_eq!(written, RECEIVE_WINDOW_FLOOR as usize);
            while server.memory.available() + written > idle {
                tokio::task::yield_now().await;
            }
            let charged = idle - server.memory.available();
            eprintln!("unadmitted peer: {written} bytes sent, {charged} bytes charged");
            assert!(charged <= 3 * RECEIVE_WINDOW_FLOOR as usize);

            let used = server.memory.limit - server.memory.available();
            let pressure = server.memory.lease(server.memory.limit / 4 * 3 - used).unwrap();
            assert!(!server.memory.has_headroom());
            assert_eq!(SendWindow::new().grow(MAX_SEND_WINDOW, &server.memory), None);
            let admitted = server.receive_credit(accepted.clone()).admit();
            let mut after_max_data = accepted.open_uni().await.unwrap();
            after_max_data.write_all(b"x").await.unwrap();
            after_max_data.finish().unwrap();
            quic.accept_uni().await.unwrap().read_to_end(1).await.unwrap();
            assert!(
                send.write(&[7]).now_or_never().is_none(),
                "window grew past the threshold"
            );

            let receiving = tokio::spawn(async move {
                let mut upload = active_server.accept_uni().await.unwrap();
                let received = upload.read_to_end(8 << 20).await.unwrap().len();
                (active_server, received)
            });
            let mut upload = active.open_uni().await.unwrap();
            upload.write_all(&vec![7; 4 << 20]).await.unwrap();
            upload.finish().unwrap();
            let (active_server, received) = receiving.await.unwrap();
            assert_eq!(received, 4 << 20);
            assert!(active_server.close_reason().is_none());

            drop((admitted, admitted_active, pressure));
            let admitted = server.receive_credit(accepted.clone()).admit();
            send.write_all(&vec![7; 1024 * 1024]).await.unwrap();
            drop(admitted);
            assert!(accepted.close_reason().is_none());
            quic.close(0_u32.into(), b"done");
            active.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn leftover_credit_closes_a_peer_that_blocks_goaway() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let (tls, mut blocking) = tls();
        let (address, stop, serving) = serve(&server, tls);
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut transport = quinn::TransportConfig::default();
        transport.receive_window((256 * 1024_u32).into());
        blocking.transport_config(Arc::new(transport));
        let post = |path: String| {
            http::Request::post(format!("https://localhost{path}"))
                .body(())
                .unwrap()
        };
        let (quic, _sender, _unread) = tokio::time::timeout(Duration::from_secs(10), async {
            let (quic, mut sender) = h3_client(&client, blocking, address).await;
            let id = upload_id(&mut sender).await;
            let mut upload = sender.send_request(post(format!("/upload?id={id}"))).await.unwrap();
            upload.send_data(Bytes::from(vec![7; 256 * 1024])).await.unwrap();
            upload.finish().await.unwrap();
            assert_eq!(upload.recv_response().await.unwrap().status(), StatusCode::OK);
            while upload.recv_data().await.unwrap().is_some() {}
            let request = http::Request::get("https://localhost/download?bytes=1073741824")
                .body(())
                .unwrap();
            let mut unread = sender.send_request(request).await.unwrap();
            unread.finish().await.unwrap();
            assert_eq!(unread.recv_response().await.unwrap().status(), StatusCode::OK);
            (quic, sender, unread)
        })
        .await
        .unwrap();
        tokio::time::pause();
        tokio::time::sleep(IDLE_TIMEOUT + SHUTDOWN_GRACE).await;
        tokio::time::resume();
        match tokio::time::timeout(Duration::from_secs(2), quic.closed()).await {
            Ok(quinn::ConnectionError::ApplicationClosed(close)) => {
                assert_eq!(close.error_code.into_inner(), h3::error::Code::H3_NO_ERROR.value())
            }
            outcome => panic!("leftover credit kept the connection: {outcome:?}"),
        }
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn peer_closes_end_connections_normally_and_protocol_errors_do_not() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let (tls, client_config) = tls();
        let endpoint =
            quinn::Endpoint::server(server.quic_config(tls).unwrap(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        tokio::time::timeout(Duration::from_secs(10), async {
            let connect = || async {
                let (peer, (accepted, from)) = tokio::join!(client.connect(address, "localhost").unwrap(), async {
                    let incoming = endpoint.accept().await.unwrap();
                    let from = incoming.remote_address();
                    (incoming.await.unwrap(), from)
                });
                (peer.unwrap(), server.clone().serve_quic_connection(accepted, from))
            };
            let (peer, served) = connect().await;
            peer.close(
                quinn::VarInt::from_u64(h3::error::Code::H3_NO_ERROR.value()).unwrap(),
                b"",
            );
            let error = served.await.unwrap_err();
            assert!(ended_normally(&*error), "{error}");

            let (peer, served) = connect().await;
            let mut control = peer.open_uni().await.unwrap();
            control.write_all(&[0x00, 0x00, 0x00]).await.unwrap();
            let error = served.await.unwrap_err();
            assert!(!ended_normally(&*error), "{error}");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn exhaustion_closes_only_the_requesting_connection() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let (tls, client_config) = tls();
        let endpoint =
            quinn::Endpoint::server(server.quic_config(tls).unwrap(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.clone().serve_quic(endpoint, async {
            let _ = stopped.await;
        }));
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        tokio::time::timeout(Duration::from_secs(10), async {
            let requesting = client.connect(address, "localhost").unwrap().await.unwrap();
            let sibling = client.connect(address, "localhost").unwrap().await.unwrap();
            let floor = connection_floor(&server.config.limits, 0);
            let filler = server
                .memory
                .lease(settled(&server.memory, &[&requesting, &sibling]).await - 4096)
                .unwrap();
            assert!(download(requesting.clone(), 64 * 1024 * 1024).await.is_err());
            match requesting.closed().await {
                quinn::ConnectionError::ConnectionClosed(close) => {
                    assert_eq!(close.error_code, quinn::TransportErrorCode::INTERNAL_ERROR)
                }
                error => panic!("unexpected close: {error:?}"),
            }
            assert!(sibling.close_reason().is_none());
            while server.memory.available() < floor {
                tokio::task::yield_now().await;
            }
            assert_eq!(download(sibling.clone(), 13).await.unwrap(), 13);
            drop(filler);
            sibling.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn silent_connections_from_few_sources_leave_budget_for_new_clients() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let (tls, client_config) = tls();
        let endpoint =
            quinn::Endpoint::server(server.quic_config(tls).unwrap(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.clone().serve_quic(endpoint, async {
            let _ = stopped.await;
        }));
        tokio::time::timeout(Duration::from_secs(8), async {
            let idle = server.memory.available();
            let mut clients = Vec::new();
            let mut connecting = Vec::new();
            for source in 2..=11 {
                let client = quinn::Endpoint::client(SocketAddr::from(([127, 0, 0, source], 0))).unwrap();
                client.set_default_client_config(client_config.clone());
                if source <= 10 {
                    for _ in 0..8 {
                        connecting.push(client.connect(address, "localhost").unwrap());
                    }
                }
                clients.push(client);
            }
            let silent = futures_util::future::join_all(connecting).await;
            assert!(silent.iter().all(Result::is_ok));
            let charged = idle - server.memory.available();
            eprintln!("{} silent connections charge {charged} bytes", silent.len());
            let floor = connection_floor(&server.config.limits, 0);
            assert!(charged < silent.len() * (floor + 64 * 1024));
            let fresh = clients[9].connect(address, "localhost").unwrap().await.unwrap();
            assert_eq!(download(fresh, 13).await.unwrap(), 13);
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn mixed_transport_exhaustion_preserves_existing_connections() {
        use super::*;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
        use tokio::net::TcpStream;
        use tokio_rustls::TlsConnector;

        tokio::time::timeout(Duration::from_secs(10), async {
            let (certificate, key) = crate::test_identity::generate_identity("localhost").unwrap();
            let certificate = CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap();
            let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
            let provider = Arc::new(crate::crypto::provider());
            let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate.clone()], key)
                .unwrap();
            tls.alpn_protocols = vec![b"h2".to_vec()];
            let floor = connection_floor(&Config::default().limits, 0);
            let server = HttpServer::with_memory(
                Arc::new(Config {
                    max_connections_per_client: 128,
                    ..Config::default()
                }),
                4 * (http_h2::BUFFER_BYTES as usize + floor),
            )
            .unwrap();
            let server = Arc::new(server);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let link = crate::test_link::Link::tcp(listener.local_addr().unwrap(), Duration::ZERO)
                .await
                .unwrap();
            link.inject(crate::test_link::Fault::None);
            let address = link.address;
            let (stop_h2, stopped_h2) = tokio::sync::oneshot::channel();
            let h2_server = tokio::spawn(server.clone().serve_http2(listener, Arc::new(tls.clone()), async {
                let _ = stopped_h2.await;
            }));
            let available = server.memory.available();
            link.inject(crate::test_link::Fault::Stall);
            let pending = TcpStream::connect(address).await.unwrap();
            while server.memory.available() == available {
                tokio::task::yield_now().await;
            }
            assert_eq!(server.memory.available(), available - http_h2::BUFFER_BYTES as usize);
            drop(pending);
            link.inject(crate::test_link::Fault::Reset);
            while server.memory.available() != available {
                tokio::task::yield_now().await;
            }
            link.inject(crate::test_link::Fault::None);
            tls.alpn_protocols = vec![b"h3".to_vec()];
            let endpoint = quinn::Endpoint::server(
                server.quic_config(Arc::new(tls)).unwrap(),
                "127.0.0.1:0".parse().unwrap(),
            )
            .unwrap();
            let quic_address = endpoint.local_addr().unwrap();
            let (stop_h3, stopped_h3) = tokio::sync::oneshot::channel();
            let h3_server = tokio::spawn(server.clone().serve_quic(endpoint, async {
                let _ = stopped_h3.await;
            }));
            let mut roots = rustls::RootCertStore::empty();
            roots.add(certificate).unwrap();
            let mut client_tls = rustls::ClientConfig::builder_with_provider(provider)
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            client_tls.alpn_protocols = vec![b"h2".to_vec()];
            let connector = TlsConnector::from(Arc::new(client_tls.clone()));
            let stream = connector
                .connect(
                    ServerName::try_from("localhost").unwrap(),
                    TcpStream::connect(address).await.unwrap(),
                )
                .await
                .unwrap();
            let (mut h2, driver) = h2::client::handshake(stream).await.unwrap();
            let h2_driver = tokio::spawn(driver);
            client_tls.alpn_protocols = vec![b"h3".to_vec()];
            let client_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
            client_endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
                quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap(),
            )));
            let unloaded = crate::test_link::Link::udp(quic_address, Duration::ZERO).await.unwrap();
            let quic = client_endpoint
                .connect(unloaded.address, "localhost")
                .unwrap()
                .await
                .unwrap();
            assert_eq!(unloaded.retries(), 0, "Retry below a quarter of the memory budget");
            let (mut driver, mut h3) = h3::client::new(h3_noq::Connection::new(quic)).await.unwrap();
            let h3_driver = tokio::spawn(async move { driver.wait_idle().await });
            let second = client_endpoint
                .connect(quic_address, "localhost")
                .unwrap()
                .await
                .unwrap();
            let admission_leases = 2 * floor + http_h2::BUFFER_BYTES as usize;
            let refundable = server.memory.limit - server.memory.available() - admission_leases;
            let exhausted = server
                .memory
                .lease(server.memory.available() + refundable + 1 - floor.min(http_h2::BUFFER_BYTES as usize))
                .unwrap();
            let pressured = crate::test_link::Link::udp(quic_address, Duration::ZERO).await.unwrap();
            assert!(
                client_endpoint
                    .connect(pressured.address, "localhost")
                    .unwrap()
                    .await
                    .is_err()
            );
            assert!(
                pressured.retries() > 0,
                "no Retry under memory pressure below the connection threshold"
            );
            assert!(
                connector
                    .connect(
                        ServerName::try_from("localhost").unwrap(),
                        TcpStream::connect(address).await.unwrap()
                    )
                    .await
                    .is_err()
            );
            let request = || {
                Request::builder()
                    .uri("https://localhost/download?bytes=4")
                    .body(())
                    .unwrap()
            };
            std::future::poll_fn(|cx| h2.poll_ready(cx)).await.unwrap();
            let (response, _) = h2.send_request(request(), true).unwrap();
            let mut response = response.await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let mut bytes = 0;
            while let Some(data) = response.body_mut().data().await {
                let data = data.unwrap();
                bytes += data.len();
                response.body_mut().flow_control().release_capacity(data.len()).unwrap();
            }
            assert_eq!(bytes, 4);
            let mut stream = h3.send_request(request()).await.unwrap();
            stream.finish().await.unwrap();
            assert_eq!(stream.recv_response().await.unwrap().status(), StatusCode::OK);
            let mut bytes = 0;
            while let Some(data) = stream.recv_data().await.unwrap() {
                bytes += bytes::Buf::remaining(&data);
            }
            assert_eq!(bytes, 4);
            stop_h2.send(()).unwrap();
            stop_h3.send(()).unwrap();
            h2_server.await.unwrap().unwrap();
            h3_server.await.unwrap().unwrap();
            h2_driver.abort();
            h3_driver.abort();
            let _ = h2_driver.await;
            let _ = h3_driver.await;
            drop((stream, exhausted));
            drop(second);
            drop(h3);
            drop(h2);
            client_endpoint.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn queued_datagrams_do_not_refuse_an_upload_stream() {
        use super::Bytes;
        use h3::quic::RecvStream as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
        use std::{future::poll_fn, sync::Arc};

        let (certificate, key) = crate::test_identity::generate_identity("localhost").unwrap();
        let certificate = CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap();
        let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
        let config = quinn::ServerConfig::with_single_cert(vec![certificate.clone()], key).unwrap();
        let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).unwrap();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(quinn::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap());
        let (sender, receiver) = tokio::join!(
            client.connect(server.local_addr().unwrap(), "localhost").unwrap(),
            async { server.accept().await.unwrap().await.unwrap() },
        );
        let sender = sender.unwrap();
        let sessions = Sessions::default();
        let (_registration, mut events) = sessions.register(0).unwrap();
        for _ in 0..1000 {
            sessions.datagram(0, Bytes::from_static(b"ping"));
        }
        let mut send = sender.open_uni().await.unwrap();
        send.write_all(b"upload").await.unwrap();
        let mut adapter = h3_noq::Connection::new(receiver);
        let recv =
            poll_fn(|cx| <h3_noq::Connection as h3::quic::Connection<Bytes>>::poll_accept_recv(&mut adapter, cx))
                .await
                .unwrap();
        sessions.stream(0, h3::stream::BufRecvStream::new(recv));
        let mut stream = events
            .streams
            .try_recv()
            .expect("upload stream was refused behind datagrams");
        assert_eq!(
            poll_fn(|cx| stream.poll_data(cx)).await.unwrap().unwrap(),
            b"upload"[..]
        );
        sender.close(0_u32.into(), b"done");
    }

    #[test]
    fn session_reservation_is_exclusive_and_released_on_drop() {
        let sessions = Sessions::default();
        let (first, _events) = sessions.register(0).expect("first session");
        assert!(sessions.register(4).is_none());
        drop(first);
        assert!(sessions.register(8).is_some());
    }
}
