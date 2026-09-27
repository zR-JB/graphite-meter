//! A QUIC connection owns its request futures; the HTTP/3 layer owns sessions and resets.

use super::*;
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::failure::LaneEnding;
use graphite_meter_http3::{self as http3, Code};
use quinn::SharedBudget;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const MIN_SEND_WINDOW: u64 = 2 * 1024 * 1024;
const MAX_SEND_WINDOW: u64 = RECEIVE_WINDOW as u64;
const SEND_WINDOW_STEP: u64 = 256 * 1024;
const SEND_WINDOW_SHRINK_DELAY: Duration = Duration::from_secs(1);
const INCOMING_BYTES: u64 = 64 * 1024;
const INCOMING_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
const UNI_STREAMS: u32 = 23;
// Go's autotuning ceilings, and one maximal 64 KiB HTTP/3 frame of credit until an upload is admitted.
const STREAM_RECEIVE_WINDOW: u32 = 32 * 1024 * 1024;
const RECEIVE_WINDOW: u32 = 48 * 1024 * 1024;
const RECEIVE_WINDOW_FLOOR: u32 = 64 * 1024;
const CREDIT_BYTES: usize = (RECEIVE_WINDOW - RECEIVE_WINDOW_FLOOR) as usize;

pub struct QuicEndpoint {
    endpoint: quinn::Endpoint,
    config: quinn::ServerConfig,
}

impl QuicEndpoint {
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    fn accept(&self, incoming: quinn::Incoming, floor: Lease) -> Option<(quinn::Connecting, Arc<ConnectionBudget>)> {
        let budget = Arc::new(ConnectionBudget {
            memory: floor.budget.clone(),
            _floor: floor,
            held: Mutex::default(),
        });
        let mut transport = (*self.config.transport).clone();
        transport.shared_budget(Some(budget.clone()));
        let mut config = self.config.clone();
        config.transport_config(Arc::new(transport));
        Some((incoming.accept_with(Arc::new(config)).ok()?, budget))
    }
}

impl HttpServer {
    pub fn quic_endpoint(
        &self,
        tls: Arc<rustls::ServerConfig>,
        address: SocketAddr,
    ) -> Result<QuicEndpoint, ConfigError> {
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
            Some(bytes),
        )?;
        let lease = Arc::new(
            self.memory
                .lease(bytes)
                .ok_or("server memory budget cannot cover QUIC endpoint buffers")?,
        );
        self.endpoint_bytes.store(bytes, Ordering::Relaxed);
        let config = self.quic_config(tls)?;
        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            endpoint_config,
            Some(config.clone()),
            Box::new(BudgetedSocket { socket, lease }),
            runtime,
        )?;
        Ok(QuicEndpoint { endpoint, config })
    }

    fn quic_config(&self, tls: Arc<rustls::ServerConfig>) -> Result<quinn::ServerConfig, ConfigError> {
        if tls.alpn_protocols != [b"h3".to_vec()] {
            return Err("HTTP/3 listener requires h3-only TLS ALPN".into());
        }
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        config
            .max_incoming(self.config.max_connections)
            .incoming_buffer_size(INCOMING_BYTES)
            .incoming_buffer_size_total(INCOMING_TOTAL_BYTES);
        let mut transport = transport(&self.config.limits)?;
        transport.shared_budget(Some(self.memory.clone()));
        config.transport_config(Arc::new(transport));
        Ok(config)
    }

    pub async fn serve_quic(
        self: Arc<Self>,
        quic: QuicEndpoint,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ConfigError> {
        tokio::pin!(shutdown);
        let mut connections = JoinSet::new();
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                Some(_) = connections.join_next() => {}
                incoming = quic.endpoint.accept() => {
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
                    let floor = connection_floor(self.handshake_bytes.load(Ordering::Relaxed));
                    let Some(floor) = self.memory.lease(floor) else {
                        incoming.refuse();
                        continue;
                    };
                    let Some((connecting, budget)) = quic.accept(incoming, floor) else { continue; };
                    let server = self.clone();
                    connections.spawn(async move {
                        let _permit = permit;
                        let quic = tokio::select! {
                            biased;
                            _ = stopped(server.stopping.clone()) => return,
                            result = tokio::time::timeout(Duration::from_secs(5), connecting) => match result {
                                Ok(Ok(quic)) => quic,
                                Ok(Err(error)) => {
                                    if !ended_normally(&error.clone().into()) {
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
                        if let Err(error) = server.clone().serve_quic_connection(quic, budget, peer).await
                            && !ended_normally(&error)
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
        quic.endpoint.close(Code::H3_NO_ERROR.into(), b"");
        connections.shutdown().await;
        let _ = tokio::time::timeout_at(drain_deadline, quic.endpoint.wait_idle()).await;
        result
    }

    /// Drives the connection until the layer ends it, which a stop does with every session's close.
    async fn serve_quic_connection(
        self: Arc<Self>,
        quic: quinn::Connection,
        budget: Arc<ConnectionBudget>,
        peer: SocketAddr,
    ) -> Result<(), http3::Error> {
        let max_requests = max_requests(&self.config.limits);
        let credit = ReceiveCredit::new(quic.clone(), budget.clone());
        let mut window = SendWindow::new();
        let mut http = http3::server::Connection::new(quic, Some(budget));
        let mut requests = FuturesUnordered::<Pin<Box<dyn Future<Output = ()> + Send>>>::new();
        let active_responses = Arc::new(AtomicUsize::new(0));
        let stopping = stopped(self.stopping.clone());
        tokio::pin!(stopping);
        let (mut shutting_down, mut closing) = (false, false);
        let close = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(close);
        let mut leftover = None;
        let stale = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(stale);
        let mut tuning = tokio::time::interval(Duration::from_millis(250));
        tuning.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let idle_since = credit.work().idle_since();
            let since = idle_since.filter(|_| credit.reserved());
            if since != leftover {
                leftover = since;
                if let Some(since) = since {
                    stale.as_mut().reset(since + IDLE_TIMEOUT);
                }
            }
            tokio::select! {
                request = http.next() => {
                    let Some(request) = request? else { return Ok(()) };
                    if requests.len() >= max_requests {
                        request.reject();
                        continue;
                    }
                    let (server, credit, active_responses) = (self.clone(), credit.clone(), active_responses.clone());
                    requests.push(Box::pin(async move {
                        let Ok((request, stream)) = request.resolve().await else { return };
                        if request.method() == Method::CONNECT {
                            let _ = server.serve_webtransport(request, stream, credit, peer).await;
                        } else {
                            let _ = server.serve_http3_request(request, stream, peer, credit, active_responses).await;
                        }
                    }));
                }
                Some(()) = requests.next() => {}
                // Level-triggered, so a connection accepted while stopping shuts down too.
                _ = &mut stopping, if !shutting_down => {
                    shutting_down = true;
                    http.shutdown(LaneEnding::Shutdown.webtransport_code(), LaneEnding::Shutdown.reason());
                }
                _ = &mut stale, if leftover.is_some() && !closing => {
                    if credit.work().idle_since() == leftover {
                        closing = true;
                        http.goaway();
                        close.as_mut().reset(tokio::time::Instant::now() + SHUTDOWN_GRACE);
                    }
                }
                // Admitted work that raced the GOAWAY runs on; the rest gets the grace.
                _ = &mut close, if closing && idle_since.is_some() => {
                    if credit.work().idle_since().is_some() { return Ok(()); }
                }
                _ = tuning.tick() => {
                    if requests.is_empty() {
                        window.release(credit.quic());
                    } else {
                        window.update(credit.quic(), &self.memory);
                    }
                }
            }
        }
    }
}

/// Go logs neither a peer's close nor an idle timeout.
fn ended_normally(error: &http3::Error) -> bool {
    use quinn::ConnectionError::{ConnectionClosed, LocallyClosed, TimedOut};
    match error {
        http3::Error::Connection { local, .. } => !local,
        http3::Error::Transport(TimedOut | LocallyClosed) => true,
        http3::Error::Transport(ConnectionClosed(close)) => close.error_code == quinn::TransportErrorCode::NO_ERROR,
        _ => false,
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

fn transport(limits: &crate::admission::Limits) -> Result<quinn::TransportConfig, ConfigError> {
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(u32::try_from(max_requests(limits))?.into());
    transport.max_concurrent_uni_streams(UNI_STREAMS.into());
    transport.stream_receive_window(STREAM_RECEIVE_WINDOW.into());
    transport.receive_window(RECEIVE_WINDOW_FLOOR.into());
    transport.send_window(MIN_SEND_WINDOW);
    transport.datagram_receive_buffer_size(Some(64 * 1024));
    transport.datagram_send_buffer_size(64 * 1024);
    transport.max_idle_timeout(Some(Duration::from_secs(30).try_into()?));
    Ok(transport)
}

/// Held from accept until Noq drops the connection: the TLS handshake and the HTTP/3 layer's fixed state.
pub(super) fn connection_floor(handshake_bytes: usize) -> usize {
    handshake_bytes.saturating_add(http3::CONNECTION_BYTES)
}

/// Noq precharges its own floor when it creates a connection, so only validation counts it.
pub(super) fn noq_floor(limits: &crate::admission::Limits) -> Result<usize, ConfigError> {
    Ok(transport(limits)?.connection_floor_bytes())
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
    held_back: AtomicBool,
}

impl MemoryBudget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            held_back: AtomicBool::new(false),
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
        let used = self.used.load(Ordering::Relaxed);
        let headroom = used < self.limit / 4 * 3;
        // Reported recovery waits for five eighths, so usage hovering at the threshold cannot flood the log.
        let held_back = self.held_back.load(Ordering::Relaxed);
        let changed = if held_back {
            used < self.limit / 8 * 5
        } else {
            !headroom
        };
        if changed
            && self
                .held_back
                .compare_exchange(held_back, !held_back, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            crate::log!(
                "[gm:memory] window growth {}: {used} of {} buffer bytes in use",
                if held_back {
                    "resumed"
                } else {
                    "held back by memory pressure"
                },
                self.limit
            );
        }
        headroom
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

/// Noq draws a connection's reserved credit first and keeps this until the connection is gone, TLS state included.
#[derive(Debug)]
struct ConnectionBudget {
    memory: Arc<MemoryBudget>,
    _floor: Lease,
    held: Mutex<Held>,
}

#[derive(Debug, Default)]
struct Held {
    credit: Option<Lease>,
    undrawn: usize,
    overdraft: usize,
}

impl ConnectionBudget {
    fn reserve(&self) -> bool {
        let mut held = self.held.lock().expect("connection budget poisoned");
        if held.credit.is_none()
            && self.memory.has_headroom()
            && let Some(credit) = self.memory.lease(CREDIT_BYTES)
        {
            held.undrawn += credit.bytes;
            held.credit = Some(credit);
        }
        held.credit.is_some()
    }
}

impl SharedBudget for ConnectionBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        let mut held = self.held.lock().expect("connection budget poisoned");
        let credit = held.undrawn.min(bytes);
        if credit < bytes && !self.memory.try_charge(bytes - credit) {
            return false;
        }
        held.undrawn -= credit;
        held.overdraft += bytes - credit;
        true
    }

    fn refund(&self, bytes: usize) {
        let mut held = self.held.lock().expect("connection budget poisoned");
        let overdraft = held.overdraft.min(bytes);
        held.overdraft -= overdraft;
        held.undrawn += bytes - overdraft;
        drop(held);
        self.memory.refund(overdraft);
    }
}

#[derive(Clone)]
pub(super) struct ReceiveCredit(Arc<CreditState>);

struct CreditState {
    quic: quinn::Connection,
    budget: Arc<ConnectionBudget>,
    work: AdmittedWork,
}

impl ReceiveCredit {
    fn new(quic: quinn::Connection, budget: Arc<ConnectionBudget>) -> Self {
        Self(Arc::new(CreditState {
            quic,
            budget,
            work: AdmittedWork::new(),
        }))
    }

    pub(super) fn quic(&self) -> &quinn::Connection {
        &self.0.quic
    }

    pub(super) fn work(&self) -> &AdmittedWork {
        &self.0.work
    }

    pub(super) fn fund(&self) -> bool {
        let reserved = self.0.budget.reserve();
        if reserved {
            self.0.quic.set_receive_window(RECEIVE_WINDOW.into());
        }
        reserved
    }

    fn reserved(&self) -> bool {
        self.0
            .budget
            .held
            .lock()
            .expect("connection budget poisoned")
            .credit
            .is_some()
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

#[cfg(test)]
mod tests {
    use super::{MAX_SEND_WINDOW, MIN_SEND_WINDOW, SendWindow, desired_send_window};
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
        (0..100).find_map(|_| {
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
        })
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

    type Requests = graphite_meter_http3::client::SendRequest;

    fn requests(quic: quinn::Connection) -> Requests {
        let (mut driver, requests) = graphite_meter_http3::client::new(quic);
        tokio::spawn(async move { driver.drive().await });
        requests
    }

    async fn download(requests: &Requests, bytes: u64) -> Result<usize, graphite_meter_http3::Error> {
        let request = http::Request::get(format!("https://localhost/download?bytes={bytes}"))
            .body(())
            .unwrap();
        let (mut send, mut recv) = requests.send_request(request).await?.split();
        send.finish().await?;
        assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
        let mut received = 0;
        while let Some(data) = recv.data().await? {
            received += data.len();
        }
        Ok(received)
    }

    fn serve(
        server: &Arc<super::HttpServer>,
        tls: Arc<rustls::ServerConfig>,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), super::ConfigError>>,
    ) {
        let quic = server.quic_endpoint(tls, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = quic.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.clone().serve_quic(quic, async {
            let _ = stopped.await;
        }));
        (address, stop, serving)
    }

    async fn h3_client(
        client: &quinn::Endpoint,
        config: quinn::ClientConfig,
        address: std::net::SocketAddr,
    ) -> (quinn::Connection, Requests) {
        let quic = client
            .connect_with(config, address, "localhost")
            .unwrap()
            .await
            .unwrap();
        (quic.clone(), requests(quic))
    }

    async fn upload_id(requests: &Requests) -> String {
        let request = http::Request::post("https://localhost/upload/session")
            .body(())
            .unwrap();
        let (mut send, mut recv) = requests.send_request(request).await.unwrap().split();
        send.finish().await.unwrap();
        assert_eq!(recv.response().await.unwrap().status(), http::StatusCode::OK);
        let mut body = Vec::new();
        while let Some(data) = recv.data().await.unwrap() {
            body.extend_from_slice(&data);
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
        let floor = connection_floor(0);
        let floors = 4 * (floor + noq_floor(&config.limits).unwrap());
        let (tls, client_config) = tls();
        let address = "127.0.0.1:0".parse().unwrap();
        let measured = HttpServer::with_memory(config.clone(), 1 << 30).unwrap();
        let idle = measured.memory.available();
        let endpoint = measured.quic_endpoint(tls.clone(), address).unwrap();
        let minimum = idle - measured.memory.available() + floors + DOWNLOAD_BLOCK_BYTES;
        drop(endpoint);
        let small = HttpServer::with_memory(config.clone(), minimum - 1).unwrap();
        assert!(
            small.cover_handshake(minimum).is_ok(),
            "chain charged QUIC floors without an endpoint"
        );
        small.cover_handshake(0).unwrap();
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
        let client = quinn::Endpoint::client(address).unwrap();
        client.set_default_client_config(client_config);
        let mut connections = Vec::new();
        for _ in 0..4 {
            let (peer, accepted) = tokio::join!(
                client.connect(endpoint.local_addr().unwrap(), "localhost").unwrap(),
                async {
                    let incoming = endpoint.endpoint.accept().await.unwrap();
                    let (connecting, budget) = endpoint.accept(incoming, server.memory.lease(floor).unwrap()).unwrap();
                    (connecting.await.unwrap(), budget)
                }
            );
            connections.push((peer.unwrap(), accepted));
        }
        assert_eq!(server.memory.available(), 0);
        assert!(server.quic_endpoint(tls, address).is_err());
        drop(connections);
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
    async fn receive_credit_is_reserved_on_grant_and_never_accumulates() {
        use super::*;
        use futures_util::FutureExt;
        let server = HttpServer::new(Arc::new(Config::default())).unwrap();
        let (tls, mut client_config) = tls();
        let mut transport = quinn::TransportConfig::default();
        transport.send_window(2 * u64::from(RECEIVE_WINDOW));
        client_config.transport_config(Arc::new(transport));
        let quic = server.quic_endpoint(tls, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = quic.local_addr().unwrap();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        let floor = connection_floor(0);
        tokio::time::timeout(Duration::from_secs(20), async {
            let connect = || async {
                let (peer, credit) = tokio::join!(client.connect(address, "localhost").unwrap(), async {
                    let incoming = quic.endpoint.accept().await.unwrap();
                    let (connecting, budget) = quic.accept(incoming, server.memory.lease(floor).unwrap()).unwrap();
                    ReceiveCredit::new(connecting.await.unwrap(), budget)
                });
                (peer.unwrap(), credit)
            };
            let fill = |peer: &quinn::Connection| {
                let peer = peer.clone();
                async move {
                    let mut total = 0;
                    loop {
                        let mut stream = peer.open_uni().await.unwrap();
                        let mut written = 0;
                        while let Some(Ok(count)) = stream.write(&[7; 64 * 1024]).now_or_never() {
                            written += count;
                        }
                        if written == 0 {
                            return total;
                        }
                        total += written;
                    }
                }
            };
            let round_trip = |peer: &quinn::Connection, credit: &ReceiveCredit| {
                let (peer, server) = (peer.clone(), credit.quic().clone());
                async move {
                    let mut probe = server.open_uni().await.unwrap();
                    probe.write_all(b"x").await.unwrap();
                    probe.finish().unwrap();
                    peer.accept_uni().await.unwrap().read_to_end(1).await.unwrap();
                }
            };
            let (peer, credit) = connect().await;
            let silent = settled(&server.memory, &[&peer]).await;
            assert_eq!(fill(&peer).await, RECEIVE_WINDOW_FLOOR as usize);
            let idle = settled(&server.memory, &[&peer]).await;
            assert!(
                silent - idle <= 3 * RECEIVE_WINDOW_FLOOR as usize,
                "unadmitted reassembly"
            );
            assert!(credit.fund(), "no grant without pressure");
            assert_eq!(
                idle - server.memory.available(),
                CREDIT_BYTES,
                "grant charged when made"
            );
            round_trip(&peer, &credit).await;
            assert_eq!(fill(&peer).await, CREDIT_BYTES);
            let charged = idle - settled(&server.memory, &[&peer]).await;
            eprintln!("{CREDIT_BYTES} bytes of credit filled: {charged} bytes charged");
            assert!(charged < CREDIT_BYTES / 4 * 5, "credit charged again as it filled");

            assert!(credit.fund());
            round_trip(&peer, &credit).await;
            assert_eq!(fill(&peer).await, 0, "a new grant added credit the peer still held");

            let used = server.memory.limit - server.memory.available();
            let pressure = server.memory.lease(server.memory.limit / 8 * 7 - used).unwrap();
            assert!(credit.fund(), "a reservation lost its window under pressure");
            let (fresh, fresh_credit) = connect().await;
            assert!(!fresh_credit.fund(), "granted under pressure");
            round_trip(&fresh, &fresh_credit).await;
            assert_eq!(fill(&fresh).await, RECEIVE_WINDOW_FLOOR as usize);
            drop(pressure);
            peer.close(0_u32.into(), b"done");
            fresh.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancelled_handshake_holds_its_floor_until_noq_drops_the_connection() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let ((identity, _), (_, distrusting)) = (tls(), tls());
        let (address, stop, serving) = serve(&server, identity);
        let floor = connection_floor(0);
        let idle = server.memory.available();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let connecting = client.connect_with(distrusting, address, "localhost").unwrap();
        assert!(connecting.await.is_err());
        tokio::time::pause();
        for _ in 0..200 {
            if server.connections.stats().active == 0 {
                break;
            }
            tokio::time::advance(Duration::from_millis(50)).await;
        }
        assert_eq!(server.connections.stats().active, 0, "handshake never timed out");
        assert!(
            idle - server.memory.available() >= floor,
            "floor refunded before Noq dropped the connection"
        );
        for _ in 0..120 {
            if server.memory.available() == idle {
                break;
            }
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        assert_eq!(server.memory.available(), idle);
        tokio::time::resume();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn probes_do_not_keep_leftover_credit_from_a_peer_that_blocks_goaway() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let (tls, mut client_config) = tls();
        let mut transport = quinn::TransportConfig::default();
        transport.receive_window(4096_u32.into());
        client_config.transport_config(Arc::new(transport));
        let (address, stop, serving) = serve(&server, tls);
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let (quic, _requests, _unread) = tokio::time::timeout(Duration::from_secs(10), async {
            let (quic, requests) = h3_client(&client, client_config, address).await;
            let id = upload_id(&requests).await;
            let request = http::Request::post(format!("https://localhost/upload?id={id}"))
                .body(())
                .unwrap();
            let (mut send, mut recv) = requests.send_request(request).await.unwrap().split();
            send.send_data(Bytes::from_static(b"granted")).await.unwrap();
            send.finish().await.unwrap();
            assert_eq!(recv.response().await.unwrap().status(), StatusCode::OK);
            while recv.data().await.unwrap().is_some() {}
            let mut unread = Vec::new();
            for _ in 0..32 {
                let request = http::Request::get("https://localhost/probe").body(()).unwrap();
                let (mut send, recv) = requests.send_request(request).await.unwrap().split();
                send.finish().await.unwrap();
                unread.push((send, recv));
            }
            settled(&server.memory, &[&quic]).await;
            (quic, requests, unread)
        })
        .await
        .unwrap();
        // Stop short of the close, so Noq drains in real time and the client sees it.
        tokio::time::pause();
        tokio::time::sleep(IDLE_TIMEOUT + SHUTDOWN_GRACE - Duration::from_secs(1)).await;
        tokio::time::resume();
        match tokio::time::timeout(Duration::from_secs(2), quic.closed()).await {
            Ok(quinn::ConnectionError::ApplicationClosed(close)) => {
                assert_eq!(close.error_code, Code::H3_NO_ERROR.into())
            }
            outcome => panic!("unread probes kept leftover credit: {outcome:?}"),
        }
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn authenticated_requests_without_admission_get_no_credit() {
        use super::*;
        let mut config = Config {
            advertised_native: Some(Default::default()),
            ..Config::default()
        };
        config.public.both.push("self".into());
        config.auth.mode = crate::config::AuthMode::Password;
        config.auth.public_url = "https://localhost".into();
        config.auth.password_hash =
            "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0".into();
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let sessions = server.auth.as_ref().unwrap().sessions();
        let (_, session) = sessions.create("subject", "Name", "local", None).unwrap();
        let (token, _grant) = sessions.issue_cli_grant(&session).unwrap();
        let (tls, mut client_config) = tls();
        let mut transport = quinn::TransportConfig::default();
        transport.stream_receive_window(16_u32.into());
        client_config.transport_config(Arc::new(transport));
        let (address, stop, serving) = serve(&server, tls);
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            let (quic, requests) = h3_client(&client, client_config, address).await;
            let idle = server.memory.available();
            let request = http::Request::get("https://localhost/probe")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ORIGIN, "")
                .body(())
                .unwrap();
            let (mut send, mut recv) = requests.send_request(request).await.unwrap().split();
            send.finish().await.unwrap();
            assert_eq!(recv.response().await.unwrap().status(), StatusCode::OK);
            while recv.data().await.unwrap().is_some() {}
            assert!(
                server.memory.available() > idle - CREDIT_BYTES / 2,
                "granted without a permit"
            );
            quic.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn peer_closes_end_connections_normally_and_protocol_errors_do_not() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let (tls, client_config) = tls();
        let quic = server.quic_endpoint(tls, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = quic.local_addr().unwrap();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        let floor = connection_floor(0);
        tokio::time::timeout(Duration::from_secs(10), async {
            let connect = || async {
                let (peer, (accepted, budget, from)) =
                    tokio::join!(client.connect(address, "localhost").unwrap(), async {
                        let incoming = quic.endpoint.accept().await.unwrap();
                        let from = incoming.remote_address();
                        let (connecting, budget) = quic.accept(incoming, server.memory.lease(floor).unwrap()).unwrap();
                        (connecting.await.unwrap(), budget, from)
                    });
                (
                    peer.unwrap(),
                    server.clone().serve_quic_connection(accepted, budget, from),
                )
            };
            let (peer, served) = connect().await;
            peer.close(Code::H3_EXCESSIVE_LOAD.into(), b"");
            let error = served.await.unwrap_err();
            assert!(ended_normally(&error), "{error}");

            let (peer, served) = connect().await;
            let mut control = peer.open_uni().await.unwrap();
            control.write_all(&[0x00, 0x00, 0x00]).await.unwrap();
            let error = served.await.unwrap_err();
            assert!(!ended_normally(&error), "{error}");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn transfers_complete_from_their_floors_when_the_budget_is_exhausted() {
        use super::*;
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let (tls, client_config) = tls();
        let endpoint = server.quic_endpoint(tls, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.clone().serve_quic(endpoint, async {
            let _ = stopped.await;
        }));
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        tokio::time::timeout(Duration::from_secs(10), async {
            let peers = [(); 2].map(|()| client.connect(address, "localhost").unwrap());
            let peers = futures_util::future::try_join_all(peers).await.unwrap();
            // New requests need layer state from the budget, so both start before it runs out.
            let mut bodies = Vec::new();
            for (peer, bytes) in peers.iter().zip([64 * 1024 * 1024, 13]) {
                let request = http::Request::get(format!("https://localhost/download?bytes={bytes}"))
                    .body(())
                    .unwrap();
                let (mut send, mut recv) = requests(peer.clone()).send_request(request).await.unwrap().split();
                send.finish().await.unwrap();
                assert_eq!(recv.response().await.unwrap().status(), StatusCode::OK);
                bodies.push((recv, bytes));
            }
            let filler = server
                .memory
                .lease(settled(&server.memory, &peers.iter().collect::<Vec<_>>()).await)
                .unwrap();
            for (mut recv, bytes) in bodies {
                let mut received = 0;
                while let Some(data) = recv.data().await.unwrap() {
                    received += data.len();
                }
                assert_eq!(received, bytes);
            }
            assert!(peers.iter().all(|peer| peer.close_reason().is_none()));
            drop(filler);
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
        let endpoint = server.quic_endpoint(tls, "127.0.0.1:0".parse().unwrap()).unwrap();
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
            let floor = connection_floor(0) + noq_floor(&server.config.limits).unwrap();
            assert!(charged <= silent.len() * floor);
            let fresh = clients[9].connect(address, "localhost").unwrap().await.unwrap();
            assert_eq!(download(&requests(fresh), 13).await.unwrap(), 13);
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
            let config = Config {
                max_connections_per_client: 128,
                ..Config::default()
            };
            let floor = connection_floor(0) + noq_floor(&config.limits).unwrap();
            let server =
                HttpServer::with_memory(Arc::new(config), 8 * (http_h2::BUFFER_BYTES as usize + floor)).unwrap();
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
            let config = server.quic_config(Arc::new(tls)).unwrap();
            let endpoint = QuicEndpoint {
                endpoint: quinn::Endpoint::server(config.clone(), "127.0.0.1:0".parse().unwrap()).unwrap(),
                config,
            };
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
            let request = || {
                Request::builder()
                    .uri("https://localhost/download?bytes=4")
                    .body(())
                    .unwrap()
            };
            // The layer charges a request's state as it arrives, so this one starts before the budget runs out.
            let (mut send, mut recv) = requests(quic.clone()).send_request(request()).await.unwrap().split();
            send.finish().await.unwrap();
            assert_eq!(recv.response().await.unwrap().status(), StatusCode::OK);
            let second = client_endpoint
                .connect(quic_address, "localhost")
                .unwrap()
                .await
                .unwrap();
            let exhausted = server.memory.lease(server.memory.available()).unwrap();
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
            let mut bytes = 0;
            while let Some(data) = recv.data().await.unwrap() {
                bytes += data.len();
            }
            assert_eq!(bytes, 4);
            assert!(quic.close_reason().is_none() && second.close_reason().is_none());
            stop_h2.send(()).unwrap();
            stop_h3.send(()).unwrap();
            h2_server.await.unwrap().unwrap();
            h3_server.await.unwrap().unwrap();
            h2_driver.abort();
            let _ = h2_driver.await;
            drop((send, recv, exhausted, second, h2));
            client_endpoint.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
    }
}
