//! A QUIC connection owns its request futures; the HTTP/3 layer owns sessions and resets.

use super::{
    lifecycle::{ConnectionLifecycle, Event},
    *,
};
use crate::{
    budget::{
        self, ClientCredit, CreditClaim, Lease, MemoryBudget, QUIC_CREDIT_BYTES, QUIC_INCOMING_BYTES,
        QUIC_INCOMING_TOTAL_BYTES, QUIC_MIN_SEND_WINDOW, QUIC_RECEIVE_WINDOW,
    },
    quic_shard,
    timeouts::QUIC_HANDSHAKE,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::failure::LaneEnding;
use graphite_meter_http3::{self as http3, Code};
use noq::SharedBudget;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_SEND_WINDOW: u64 = 16 * 1024 * 1024;
const SEND_WINDOW_STEP: u64 = 256 * 1024;
const SEND_WINDOW_SHRINK_DELAY: Duration = Duration::from_secs(1);
/// How often a connection's send window follows its demand.
const SEND_WINDOW_TUNING: Duration = Duration::from_millis(250);

pub struct QuicEndpoint {
    endpoint: noq::Endpoint,
    config: noq::ServerConfig,
    clients: Arc<ClientCredit>,
}

impl QuicEndpoint {
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    fn accept(&self, incoming: noq::Incoming, floor: Lease) -> Option<(noq::Connecting, Arc<ConnectionBudget>)> {
        let budget = Arc::new(ConnectionBudget {
            memory: floor.budget.clone(),
            clients: self.clients.clone(),
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
    ) -> Result<QuicEndpoint, ServerError> {
        let udp = bind_udp(address, 1)?;
        let endpoint_config = noq::EndpointConfig::default();
        let bytes = budget::endpoint_bytes(
            &endpoint_config,
            1,
            self.config.max_connections,
            udp.kernel_bytes,
            udp.socket.max_receive_segments().get(),
        )
        .ok_or("QUIC endpoint buffer size overflow")?;
        budget::check(
            &self.config,
            self.memory.limit,
            self.handshake_bytes.load(Ordering::Relaxed),
            Some(bytes),
        )?;
        let config = self.quic_config(tls, 1)?;
        let quic = self.endpoint(endpoint_config, &config, udp.socket, udp.runtime, bytes)?;
        self.memory.reserved.store(bytes, Ordering::Relaxed);
        Ok(quic)
    }

    /// Endpoints on `address` for as many of these runtimes as the buffer budget covers, all bound with
    /// `SO_REUSEPORT`, each socket and endpoint built in its own runtime; `None` when fewer than two fit.
    /// They share one token key, reset key and every server-wide limit.
    pub(crate) fn quic_shards(
        &self,
        tls: Arc<rustls::ServerConfig>,
        address: SocketAddr,
        runtimes: &[tokio::runtime::Handle],
    ) -> Result<Option<Vec<QuicEndpoint>>, ServerError> {
        const OVERFLOW: &str = "QUIC endpoint buffer size overflow";
        let bind = |runtime: &tokio::runtime::Handle, address| {
            let _entered = runtime.enter();
            bind_udp(address, runtimes.len())
        };
        let Some(runtime) = runtimes.first() else {
            return Ok(None);
        };
        let first = bind(runtime, address)?;
        // The others join the first socket's port, which the OS picks for port 0.
        let address = first.socket.local_addr()?;
        let endpoint_config = noq::EndpointConfig::default();
        let handshake_bytes = self.handshake_bytes.load(Ordering::Relaxed);
        let shard_bytes = |shards, udp: &Udp| {
            let segments = udp.socket.max_receive_segments().get();
            budget::endpoint_bytes(
                &endpoint_config,
                shards,
                self.config.max_connections,
                udp.kernel_bytes,
                segments,
            )
        };
        let check = |total| budget::check(&self.config, self.memory.limit, handshake_bytes, Some(total));
        // The first socket stands for the others until they are bound; the check below counts each.
        let covered = |shards: usize| {
            shard_bytes(shards, &first)
                .and_then(|bytes| bytes.checked_mul(shards))
                .is_some_and(|total| check(total).is_ok())
        };
        let Some(shards) = (2..=runtimes.len()).rev().find(|&shards| covered(shards)) else {
            return Ok(None);
        };
        let mut sockets = vec![first];
        for runtime in &runtimes[1..shards] {
            sockets.push(bind(runtime, address)?);
        }
        let bytes = sockets
            .iter()
            .map(|udp| shard_bytes(shards, udp))
            .collect::<Option<Vec<_>>>()
            .ok_or(OVERFLOW)?;
        let total = bytes
            .iter()
            .try_fold(0_usize, |total, &bytes| total.checked_add(bytes))
            .ok_or(OVERFLOW)?;
        check(total)?;
        let config = self.quic_config(tls, shards)?;
        let (router, inboxes) =
            quic_shard::Router::new(shards, budget::packet_bytes(&endpoint_config).ok_or(OVERFLOW)?);
        let mut endpoints = Vec::with_capacity(shards);
        for (shard, ((udp, bytes), inbox)) in sockets.into_iter().zip(bytes).zip(inboxes).enumerate() {
            let socket = Box::new(quic_shard::ShardSocket::new(udp.socket, shard, router.clone(), inbox));
            let mut shard_config = endpoint_config.clone();
            shard_config.cid_generator(quic_shard::cid_generator(u8::try_from(shard)?));
            let _entered = runtimes[shard].enter();
            endpoints.push(self.endpoint(shard_config, &config, socket, udp.runtime, bytes)?);
        }
        self.memory.reserved.store(total, Ordering::Relaxed);
        Ok(Some(endpoints))
    }

    /// An endpoint on `socket`, whose buffers hold `bytes` of the budget for as long as it runs.
    fn endpoint(
        &self,
        endpoint_config: noq::EndpointConfig,
        config: &noq::ServerConfig,
        socket: Box<dyn noq::AsyncUdpSocket>,
        runtime: Arc<dyn noq::Runtime>,
        bytes: usize,
    ) -> Result<QuicEndpoint, ServerError> {
        let lease = self
            .memory
            .lease(bytes)
            .ok_or("server memory budget cannot cover QUIC endpoint buffers")?;
        let socket = Box::new(BudgetedSocket {
            socket,
            lease: Arc::new(lease),
        });
        let endpoint = noq::Endpoint::new_with_abstract_socket(endpoint_config, Some(config.clone()), socket, runtime)?;
        Ok(QuicEndpoint {
            endpoint,
            config: config.clone(),
            clients: self.client_credit.clone(),
        })
    }

    /// One of `shards` endpoints admits its part of the server-wide incoming limits, rounded up.
    fn quic_config(&self, tls: Arc<rustls::ServerConfig>, shards: usize) -> Result<noq::ServerConfig, ServerError> {
        let mut tls = (*tls).clone();
        tls.alpn_protocols = vec![topology::QUIC.alpn.to_vec()];
        let crypto = noq::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let mut config = noq::ServerConfig::with_crypto(Arc::new(crypto));
        config
            .max_incoming(self.config.max_connections.div_ceil(shards))
            // Every Initial of a handshake reaches the endpoint that holds it, so its limit is not split.
            .incoming_buffer_size(QUIC_INCOMING_BYTES)
            .incoming_buffer_size_total(QUIC_INCOMING_TOTAL_BYTES.div_ceil(shards as u64));
        let mut transport = budget::quic_transport(&self.config.limits)?;
        transport.shared_budget(Some(self.memory.clone()));
        config.transport_config(Arc::new(transport));
        Ok(config)
    }

    pub async fn serve_quic(
        self: Arc<Self>,
        quic: QuicEndpoint,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), ServerError> {
        tokio::pin!(shutdown);
        let mut connections = JoinSet::new();
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                Some(_) = connections.join_next() => {}
                incoming = quic.endpoint.accept() => {
                    let Some(incoming) = incoming else { break Ok(()); };
                    let peer = incoming.remote_address();
                    // Retry spends a round trip to protect admission under load, and, as in Go, a source's QUIC
                    // share from Initials that may be spoofed: only its first connection skips it.
                    if !incoming.remote_address_validated()
                        && (self.connections.stats().active >= self.config.max_connections / 4
                            || self.memory.under_pressure()
                            || self.connections.holds_quic(peer))
                    {
                        let _ = incoming.retry();
                        continue;
                    }
                    let Some(permit) = self.connections.acquire(peer, true) else {
                        incoming.refuse();
                        continue;
                    };
                    let floor = budget::connection_floor(self.handshake_bytes.load(Ordering::Relaxed));
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
                            result = tokio::time::timeout(QUIC_HANDSHAKE, connecting) => match result {
                                Ok(Ok(quic)) => quic,
                                Ok(Err(error)) => {
                                    // A peer's close reason is its own text: quoted, as Go quotes connection errors.
                                    if !ended_normally(&error.clone().into()) {
                                        server.peers.write(format_args!("[gm:h3] QUIC handshake error from {}: {:?}", peer.ip().to_canonical(), error.to_string()));
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
        quic: noq::Connection,
        budget: Arc<ConnectionBudget>,
        peer: SocketAddr,
    ) -> Result<(), http3::Error> {
        let max_requests = budget::max_requests(&self.config.limits).expect("validated stream budgets") as usize;
        let credit = ReceiveCredit::new(quic.clone(), budget.clone());
        let mut window = SendWindow::new();
        let mut http = http3::server::Connection::new(quic, Some(budget));
        let mut requests = FuturesUnordered::new();
        let active_responses = Arc::new(AtomicUsize::new(0));
        let stopping = stopped(self.stopping.clone());
        tokio::pin!(stopping);
        let mut shutting_down = false;
        let mut lifecycle = ConnectionLifecycle::new(credit.work().clone(), false);
        let mut tuning = tokio::time::interval(SEND_WINDOW_TUNING);
        tuning.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                request = http.next() => {
                    let Some(request) = request? else { return Ok(()) };
                    if requests.len() >= max_requests {
                        request.reject();
                        continue;
                    }
                    let (server, credit, active_responses) = (self.clone(), credit.clone(), active_responses.clone());
                    requests.push(async move {
                        let Ok((request, stream)) = request.resolve().await else { return };
                        if request.method() == Method::CONNECT {
                            let _ = server.serve_webtransport(request, stream, credit, peer).await;
                        } else {
                            let _ = server.serve_http3_request(request, stream, peer, credit, active_responses).await;
                        }
                    });
                }
                Some(()) = requests.next() => {}
                // Level-triggered, so a connection accepted while stopping shuts down too.
                _ = &mut stopping, if !shutting_down => {
                    shutting_down = true;
                    http.shutdown(LaneEnding::Shutdown.webtransport_code(), LaneEnding::Shutdown.reason());
                }
                // Admitted work that raced the GOAWAY runs on; the rest gets the grace.
                event = std::future::poll_fn(|cx| lifecycle.poll(cx, || credit.reserved())) => match event {
                    Event::GoAway => http.goaway(),
                    Event::Close => return Ok(()),
                },
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
    use noq::ConnectionError::{ConnectionClosed, LocallyClosed, TimedOut};
    match error {
        http3::Error::Connection { local, .. } => !local,
        http3::Error::Transport(TimedOut | LocallyClosed) => true,
        http3::Error::Transport(ConnectionClosed(close)) => close.error_code == noq::TransportErrorCode::NO_ERROR,
        _ => false,
    }
}

/// One of `sockets` UDP sockets on an address, bound in the current runtime, and the kernel buffer bytes it holds.
struct Udp {
    socket: Box<dyn noq::AsyncUdpSocket>,
    runtime: Arc<dyn noq::Runtime>,
    kernel_bytes: usize,
}

fn bind_udp(address: SocketAddr, sockets: usize) -> Result<Udp, ServerError> {
    let (socket, warning) = graphite_meter_core::socket::udp_socket_with(address, sockets)?;
    if let Some(warning) = warning {
        crate::log!("{warning}");
    }
    let socket_buffers = socket2::SockRef::from(&socket);
    let kernel_bytes = socket_buffers
        .recv_buffer_size()?
        .checked_add(socket_buffers.send_buffer_size()?)
        .ok_or("UDP socket buffer size overflow")?;
    let runtime = noq::default_runtime().ok_or("no async runtime for QUIC")?;
    let socket = runtime.wrap_udp_socket(socket)?;
    Ok(Udp {
        socket,
        runtime,
        kernel_bytes,
    })
}

#[derive(Debug)]
struct BudgetedSocket {
    socket: Box<dyn noq::AsyncUdpSocket>,
    lease: Arc<Lease>,
}

impl noq::AsyncUdpSocket for BudgetedSocket {
    fn create_sender(&self) -> Pin<Box<dyn noq::UdpSender>> {
        Box::pin(BudgetedSender {
            sender: self.socket.create_sender(),
            _lease: self.lease.clone(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [noq::udp::RecvMeta],
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
    sender: Pin<Box<dyn noq::UdpSender>>,
    _lease: Arc<Lease>,
}

impl noq::UdpSender for BudgetedSender {
    fn poll_send(
        mut self: Pin<&mut Self>,
        transmit: &noq::udp::Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        self.sender.as_mut().poll_send(transmit, cx)
    }

    fn max_transmit_segments(&self) -> std::num::NonZeroUsize {
        self.sender.max_transmit_segments()
    }
}

/// Noq draws a connection's reserved credit first and keeps this until the connection is gone, TLS state included.
#[derive(Debug)]
struct ConnectionBudget {
    memory: Arc<MemoryBudget>,
    clients: Arc<ClientCredit>,
    _floor: Lease,
    held: Mutex<Held>,
}

#[derive(Debug, Default)]
struct Held {
    credit: Option<Lease>,
    /// The share of the client whose admitted upload first funded the window.
    claim: Option<CreditClaim>,
    undrawn: usize,
    overdraft: usize,
}

impl ConnectionBudget {
    /// Reserves the window once, from the budget and from the admitted client's share of it.
    fn reserve(&self, clients: &[String]) -> bool {
        let mut held = lock(&self.held);
        if held.credit.is_none()
            && self.memory.has_headroom()
            && let Some(claim) = self.clients.claim(clients, QUIC_CREDIT_BYTES)
            && let Some(credit) = self.memory.lease(QUIC_CREDIT_BYTES)
        {
            held.undrawn += credit.bytes;
            held.credit = Some(credit);
            held.claim = Some(claim);
        }
        held.credit.is_some()
    }
}

impl SharedBudget for ConnectionBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        let mut held = lock(&self.held);
        let credit = held.undrawn.min(bytes);
        if credit < bytes && !self.memory.try_charge(bytes - credit) {
            return false;
        }
        held.undrawn -= credit;
        held.overdraft += bytes - credit;
        true
    }

    fn refund(&self, bytes: usize) {
        let mut held = lock(&self.held);
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
    quic: noq::Connection,
    budget: Arc<ConnectionBudget>,
    work: AdmittedWork,
}

impl ReceiveCredit {
    fn new(quic: noq::Connection, budget: Arc<ConnectionBudget>) -> Self {
        Self(Arc::new(CreditState {
            quic,
            budget,
            work: AdmittedWork::new(),
        }))
    }

    pub(super) fn quic(&self) -> &noq::Connection {
        &self.0.quic
    }

    pub(super) fn work(&self) -> &AdmittedWork {
        &self.0.work
    }

    /// `clients` are the admitted upload's keys, whose share the window is charged to.
    pub(super) fn fund(&self, clients: &[String]) -> bool {
        let reserved = self.0.budget.reserve(clients);
        if reserved {
            self.0.quic.set_receive_window(QUIC_RECEIVE_WINDOW.into());
        }
        reserved
    }

    fn reserved(&self) -> bool {
        lock(&self.0.budget.held).credit.is_some()
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
            limit: QUIC_MIN_SEND_WINDOW,
            last: None,
            low_demand_since: None,
        }
    }

    fn update(&mut self, connection: &noq::Connection, budget: &MemoryBudget) {
        // Read the aggregate first so a concurrent send on the initial path
        // cannot look like traffic on another path.
        let all_sent = connection.stats().udp_tx.bytes;
        let Some(path) = connection.path_stats(noq::PathId::ZERO) else {
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
            // Two observed bandwidth-delay products let a new path grow without a full window on fast local
            // links, up to about quic-go's in-flight cap: a deeper window only fills the bottleneck queue.
            let target = desired_send_window(sent.saturating_sub(previous), path.rtt, now.duration_since(last));
            if target == QUIC_MIN_SEND_WINDOW && self.limit != QUIC_MIN_SEND_WINDOW {
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

    fn release(&mut self, connection: &noq::Connection) {
        self.last = None;
        self.low_demand_since = None;
        if self.limit != QUIC_MIN_SEND_WINDOW {
            self.limit = QUIC_MIN_SEND_WINDOW;
            connection.set_send_window(QUIC_MIN_SEND_WINDOW);
        }
    }
}

fn desired_send_window(sent: u64, rtt: Duration, elapsed: Duration) -> u64 {
    let Some(demand) = u128::from(sent)
        .saturating_mul(rtt.as_nanos())
        .saturating_mul(2)
        .checked_div(elapsed.as_nanos())
    else {
        return QUIC_MIN_SEND_WINDOW;
    };
    demand.clamp(u128::from(QUIC_MIN_SEND_WINDOW), u128::from(MAX_SEND_WINDOW)) as u64
}

#[cfg(test)]
mod tests {
    use super::{MAX_SEND_WINDOW, QUIC_MIN_SEND_WINDOW, SendWindow};
    use crate::{
        budget::{QUIC_RECEIVE_WINDOW_FLOOR, connection_floor, noq_floor},
        config::Config,
    };
    use std::{sync::Arc, time::Duration};

    fn tls() -> (Arc<rustls::ServerConfig>, noq::ClientConfig) {
        tls_offering(vec![b"h3".to_vec()])
    }

    /// An identity for localhost, and a client that trusts it and offers `alpn`.
    fn tls_offering(alpn: Vec<Vec<u8>>) -> (Arc<rustls::ServerConfig>, noq::ClientConfig) {
        let (tls, mut client) = crate::test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"h3"]).unwrap();
        client.alpn_protocols = alpn;
        let client = noq::ClientConfig::new(Arc::new(
            noq::crypto::rustls::QuicClientConfig::try_from(client).unwrap(),
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

    async fn settled(memory: &super::MemoryBudget, peers: &[&noq::Connection]) -> usize {
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

    fn requests(quic: noq::Connection) -> Requests {
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
        tokio::task::JoinHandle<Result<(), super::ServerError>>,
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
        client: &noq::Endpoint,
        config: noq::ClientConfig,
        address: std::net::SocketAddr,
    ) -> (noq::Connection, Requests) {
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
        let config = Config {
            max_connections: 4,
            max_connections_per_client: 4,
            ..Config::default()
        }
        .validated()
        .unwrap();
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
        let client = noq::Endpoint::client(address).unwrap();
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
        use noq::AsyncUdpSocket;
        let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let runtime = noq::default_runtime().unwrap();
        let socket = runtime
            .wrap_udp_socket(
                graphite_meter_core::socket::udp_socket("127.0.0.1:0".parse().unwrap())
                    .unwrap()
                    .0,
            )
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
        let transmit = noq::udp::Transmit {
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

    #[tokio::test]
    async fn idle_send_window_shrinks_while_control_stream_stays_open() {
        let (tls, client_config) = tls();
        let config =
            noq::ServerConfig::with_crypto(Arc::new(noq::crypto::rustls::QuicServerConfig::try_from(tls).unwrap()));
        let server = noq::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
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
            assert_eq!(window.limit, QUIC_MIN_SEND_WINDOW);
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
        let server = HttpServer::new(Config::default().validated().unwrap()).unwrap();
        let (tls, mut client_config) = tls();
        let mut transport = noq::TransportConfig::default();
        transport.send_window(2 * u64::from(QUIC_RECEIVE_WINDOW));
        client_config.transport_config(Arc::new(transport));
        let quic = server.quic_endpoint(tls, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = quic.local_addr().unwrap();
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
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
            let fill = |peer: &noq::Connection| {
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
            let round_trip = |peer: &noq::Connection, credit: &ReceiveCredit| {
                let (peer, server) = (peer.clone(), credit.quic().clone());
                async move {
                    let mut probe = server.open_uni().await.unwrap();
                    probe.write_all(b"x").await.unwrap();
                    probe.finish().unwrap();
                    peer.accept_uni().await.unwrap().read_to_end(1).await.unwrap();
                }
            };
            let clients = crate::client_address::client_keys("127.0.0.1".parse().unwrap());
            let (peer, credit) = connect().await;
            let silent = settled(&server.memory, &[&peer]).await;
            assert_eq!(fill(&peer).await, QUIC_RECEIVE_WINDOW_FLOOR as usize);
            let idle = settled(&server.memory, &[&peer]).await;
            assert!(
                silent - idle <= 3 * QUIC_RECEIVE_WINDOW_FLOOR as usize,
                "unadmitted reassembly"
            );
            assert!(credit.fund(&clients), "no grant without pressure");
            assert_eq!(
                idle - server.memory.available(),
                QUIC_CREDIT_BYTES,
                "grant charged when made"
            );
            round_trip(&peer, &credit).await;
            assert_eq!(fill(&peer).await, QUIC_CREDIT_BYTES);
            let charged = idle - settled(&server.memory, &[&peer]).await;
            eprintln!("{QUIC_CREDIT_BYTES} bytes of credit filled: {charged} bytes charged");
            assert!(charged < QUIC_CREDIT_BYTES / 4 * 5, "credit charged again as it filled");

            assert!(credit.fund(&clients));
            round_trip(&peer, &credit).await;
            assert_eq!(fill(&peer).await, 0, "a new grant added credit the peer still held");

            let used = server.memory.limit - server.memory.available();
            let pressure = server.memory.lease(server.memory.limit / 8 * 7 - used).unwrap();
            assert!(credit.fund(&clients), "a reservation lost its window under pressure");
            let (fresh, fresh_credit) = connect().await;
            assert!(!fresh_credit.fund(&clients), "granted under pressure");
            round_trip(&fresh, &fresh_credit).await;
            assert_eq!(fill(&fresh).await, QUIC_RECEIVE_WINDOW_FLOOR as usize);
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
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let ((identity, _), (_, distrusting)) = (tls(), tls());
        let (address, stop, serving) = serve(&server, identity);
        let floor = connection_floor(0);
        let idle = server.memory.available();
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let connecting = client.connect_with(distrusting, address, "localhost").unwrap();
        assert!(connecting.await.is_err());
        tokio::time::pause();
        for _ in 0..QUIC_HANDSHAKE.as_millis() / 25 {
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

    /// As quic-go's, a handshake has ten seconds in all.
    #[tokio::test]
    async fn a_handshake_has_ten_seconds_in_all() {
        use super::*;
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let (tls, client_config) = tls();
        let (address, stop, serving) = serve(&server, tls);
        // A client's first Initial, from a socket that never answers the server.
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let _connecting = client
            .connect_with(client_config, silent.local_addr().unwrap(), "localhost")
            .unwrap();
        let mut initial = [0; 1500];
        let length = silent.recv(&mut initial).await.unwrap();
        silent.send_to(&initial[..length], address).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while server.connections.stats().active == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::pause();
        let wait = async |seconds| {
            for _ in 0..seconds * 10 {
                tokio::time::advance(Duration::from_millis(100)).await;
            }
        };
        // Real time passed since the handshake began, so the first check leaves it a margin.
        wait(8).await;
        assert_eq!(
            server.connections.stats().active,
            1,
            "the handshake ended before ten seconds"
        );
        wait(3).await;
        assert_eq!(
            server.connections.stats().active,
            0,
            "the handshake outlived ten seconds"
        );
        tokio::time::resume();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn probes_do_not_keep_leftover_credit_from_a_peer_that_blocks_goaway() {
        use super::*;
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let (tls, mut client_config) = tls();
        let mut transport = noq::TransportConfig::default();
        transport.receive_window(4096_u32.into());
        client_config.transport_config(Arc::new(transport));
        let (address, stop, serving) = serve(&server, tls);
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
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
        tokio::time::sleep(CONTROL + SHUTDOWN_GRACE - Duration::from_secs(1)).await;
        tokio::time::resume();
        match tokio::time::timeout(Duration::from_secs(2), quic.closed()).await {
            Ok(noq::ConnectionError::ApplicationClosed(close)) => {
                assert_eq!(close.error_code, Code::H3_NO_ERROR.into())
            }
            outcome => panic!("unread probes kept leftover credit: {outcome:?}"),
        }
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn authenticated_controls_take_no_receive_credit_and_webtransport_is_hardened() {
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
        let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
        let sessions = server.auth.as_ref().unwrap().sessions();
        let (_, session) = sessions.create("subject", "Name", "local", None).unwrap();
        let (token, _grant) = sessions.issue_cli_grant(&session).unwrap();
        let (tls, mut client_config) = tls();
        let mut transport = noq::TransportConfig::default();
        transport.stream_receive_window(16_u32.into());
        client_config.transport_config(Arc::new(transport));
        let (address, stop, serving) = serve(&server, tls);
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
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
                server.memory.available() > idle - QUIC_CREDIT_BYTES / 2,
                "granted without a permit"
            );
            let request = http::Request::get("https://localhost/wt/ping")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ORIGIN, "")
                .body(())
                .unwrap();
            let (session, response) = http3::webtransport::Session::connect(&requests, request)
                .await
                .unwrap()
                .expect("an authorized session");
            let headers = response.headers();
            assert_eq!(headers[header::STRICT_TRANSPORT_SECURITY], "max-age=31536000");
            assert_eq!(headers["referrer-policy"], "same-origin");
            assert_eq!(headers["x-content-type-options"], "nosniff");
            assert!(headers.contains_key("permissions-policy"));
            session.close(0, "").await;
            quic.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn transfers_complete_from_their_floors_when_the_budget_is_exhausted() {
        use super::*;
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let (tls, client_config) = tls();
        let endpoint = server.quic_endpoint(tls, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.clone().serve_quic(endpoint, async {
            let _ = stopped.await;
        }));
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        tokio::time::timeout(Duration::from_secs(10), async {
            let peers = [(); 2].map(|()| client.connect(address, "localhost").unwrap());
            let peers = futures_util::future::try_join_all(peers).await.unwrap();
            // New requests need layer state from the budget, so both start before it runs out. The larger body is
            // four times the server's first send window, so most of it moves after the budget runs out.
            let mut bodies = Vec::new();
            for (peer, bytes) in peers.iter().zip([4 * QUIC_MIN_SEND_WINDOW as usize, 13]) {
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
    async fn one_client_holds_at_most_its_share_of_receive_credit() {
        use super::*;
        // A client's share is a window on each QUIC connection it may hold, as Go grants each its window. While its
        // other connections hold all of it but one window, one more fits it and the next does not.
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let keys = crate::client_address::client_keys([127, 0, 0, 1].into());
        let others = (crate::connections::QUIC_PER_CLIENT - 1) * QUIC_CREDIT_BYTES;
        let _others = server.client_credit.claim(&keys, others).unwrap();
        let (tls, client_config) = tls();
        let (address, stop, serving) = serve(&server, tls);
        tokio::time::timeout(Duration::from_secs(20), async {
            let mut held = Vec::new();
            let mut charged = Vec::new();
            for source in [1, 1, 2] {
                let client = noq::Endpoint::client(SocketAddr::from(([127, 0, 0, source], 0))).unwrap();
                let (quic, requests) = h3_client(&client, client_config.clone(), address).await;
                let id = upload_id(&requests).await;
                let before = settled(&server.memory, &[&quic]).await;
                let request = http::Request::post(format!("https://localhost/upload?id={id}"))
                    .body(())
                    .unwrap();
                let (mut send, mut recv) = requests.send_request(request).await.unwrap().split();
                send.send_data(Bytes::from_static(b"funded")).await.unwrap();
                send.finish().await.unwrap();
                assert_eq!(recv.response().await.unwrap().status(), StatusCode::OK);
                while recv.data().await.unwrap().is_some() {}
                charged.push(before - settled(&server.memory, &[&quic]).await);
                held.push((client, quic, requests));
            }
            // One HTTP/3 window fits the rest of 127.0.0.1's share and a second does not; 127.0.0.2's own share
            // still funds one.
            let funded: Vec<_> = charged.iter().map(|&bytes| bytes >= QUIC_CREDIT_BYTES).collect();
            assert_eq!(funded, [true, false, true], "{charged:?}");
            for (_, quic, _) in held {
                quic.close(0_u32.into(), b"done");
            }
        })
        .await
        .unwrap();
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stalled_http3_replies_end_at_the_control_and_idle_bounds() {
        use super::*;
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let (tls, mut client_config) = tls();
        let mut transport = noq::TransportConfig::default();
        // Too little stream credit for any reply's head; pings keep the stalled connection open.
        transport.stream_receive_window(64_u32.into());
        transport.keep_alive_interval(Some(Duration::from_secs(5)));
        client_config.transport_config(Arc::new(transport));
        let (address, stop, serving) = serve(&server, tls);
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let (quic, requests) = tokio::time::timeout(Duration::from_secs(5), h3_client(&client, client_config, address))
            .await
            .unwrap();
        let open = async |path: &str| {
            let request = http::Request::get(format!("https://localhost{path}")).body(()).unwrap();
            let (mut send, recv) = requests.send_request(request).await.unwrap().split();
            send.finish().await.unwrap();
            (send, recv)
        };
        let (_probe, mut probe) = open("/probe").await;
        let (_download, mut download) = open("/download?bytes=1000000").await;
        settled(&server.memory, &[&quic]).await;
        assert_eq!(server.admission.load().0, 1, "the download holds its permit");
        // Reading a reply would grant it credit, so each is read only past its bound.
        let step = async || {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(8)).await;
            tokio::time::resume();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        step().await;
        step().await;
        let reply = tokio::time::timeout(Duration::from_secs(2), probe.response()).await;
        assert!(
            matches!(reply, Ok(Err(_))),
            "an unadmitted reply outlived Go's fifteen seconds: {reply:?}"
        );
        step().await;
        step().await;
        let reply = tokio::time::timeout(Duration::from_secs(2), download.response()).await;
        assert!(
            matches!(reply, Ok(Err(_))),
            "a stalled download outlived its thirty idle seconds: {reply:?}"
        );
        assert_eq!(server.admission.load().0, 0);
        quic.close(0_u32.into(), b"done");
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn silent_connections_from_few_sources_leave_budget_for_new_clients() {
        use super::*;
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
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
                let client = noq::Endpoint::client(SocketAddr::from(([127, 0, 0, source], 0))).unwrap();
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

    /// Go's and browsers' post-quantum ClientHello spans Initials that arrive while the handshake waits to be
    /// accepted; one of many endpoints keeps them all.
    #[tokio::test]
    async fn a_waiting_handshake_keeps_every_initial_on_one_of_many_endpoints() {
        use super::*;
        let server = HttpServer::new(Config::default().validated().unwrap()).unwrap();
        // Protocols the server does not speak spread this ClientHello over six Initials.
        let unspoken = (0..256).map(|index| format!("unspoken-protocol-{index:03}").into_bytes());
        let (tls, client_config) = tls_offering(unspoken.chain([b"h3".to_vec()]).collect());
        let config = server.quic_config(tls, 16).unwrap();
        let endpoint = QuicEndpoint {
            endpoint: noq::Endpoint::server(config.clone(), "127.0.0.1:0".parse().unwrap()).unwrap(),
            config,
            clients: server.client_credit.clone(),
        };
        let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = endpoint.local_addr().unwrap();
        let connecting = client.connect_with(client_config, address, "localhost").unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            let incoming = endpoint.endpoint.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let floor = server.memory.lease(connection_floor(0)).unwrap();
            let (accepting, _budget) = endpoint.accept(incoming, floor).unwrap();
            let (connected, accepted) = tokio::join!(connecting, accepting);
            let (connected, _accepted) = (connected.unwrap(), accepted.unwrap());
            assert_eq!(connected.stats().lost_packets, 0, "an Initial was dropped");
        })
        .await
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn four_workers_run_two_shards_that_follow_rebound_clients() {
        use super::*;
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let (tls, client_config) = tls();
        let bound = crate::runtime::Quic::bind(&server, tls, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = bound.local_addr().unwrap();
        let crate::runtime::Quic::Shards(shards) = &bound else {
            panic!("four runtime workers served HTTP/3 from one endpoint");
        };
        assert_eq!(shards.len(), 2);
        // A shard counts the handshakes it accepts, so a new connection shows where the kernel hands its address.
        let endpoints: Vec<_> = shards.iter().map(|(_, quic)| quic.endpoint.clone()).collect();
        let handshakes = || endpoints.iter().map(|endpoint| endpoint.stats().accepted_handshakes);
        let grew = |before: Vec<u64>| handshakes().zip(before).position(|(now, then)| now > then).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let serving = tokio::spawn(futures_util::future::try_join_all(bound.serve(&server, &stopped)));
        tokio::time::timeout(Duration::from_secs(30), async {
            // Distinct sources stand for distinct clients, whose 4-tuples the kernel spreads over the shards.
            let transfers = (2..10).map(|source| {
                let config = client_config.clone();
                async move {
                    let client = noq::Endpoint::client(SocketAddr::from(([127, 0, 0, source], 0))).unwrap();
                    let (quic, requests) = h3_client(&client, config, address).await;
                    let received = download(&requests, 1 << 20).await.unwrap();
                    quic.close(0_u32.into(), b"done");
                    received
                }
            });
            assert_eq!(futures_util::future::join_all(transfers).await, [1 << 20; 8]);

            let client = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
            let before = handshakes().collect();
            let (quic, requests) = h3_client(&client, client_config.clone(), address).await;
            let home = grew(before);
            assert_eq!(download(&requests, 13).await.unwrap(), 13);
            // A new port lands on the other shard one time in two, whose socket must forward to the connection's.
            for rebind in 1..32 {
                client
                    .rebind(std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
                    .unwrap();
                let before = handshakes().collect();
                let probe = client
                    .connect_with(client_config.clone(), address, "localhost")
                    .unwrap();
                probe.await.unwrap().close(0_u32.into(), b"done");
                let crossed = grew(before) != home;
                let received = download(&requests, 64 * 1024).await.unwrap();
                assert_eq!(received, 64 * 1024, "after rebind {rebind}");
                if crossed {
                    break;
                }
                assert!(rebind < 31, "{rebind} rebinds never left the connection's shard");
            }
            quic.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
        stop.send_replace(true);
        serving.await.unwrap().unwrap();
    }

    /// The shards' reservations grow with the host's cores, not its load, so an idle server is not under pressure
    /// however much of the budget they take.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 40)]
    async fn an_idle_server_with_many_shards_is_not_under_pressure() {
        use super::*;
        let config = Config {
            max_connections: 4,
            max_connections_per_client: 4,
            ..Config::default()
        }
        .validated()
        .unwrap();
        let floors = 4 * (connection_floor(0) + noq_floor(&config.limits).unwrap()) + DOWNLOAD_BLOCK_BYTES;
        let (tls, _) = tls();
        let bind = |memory| {
            let server = HttpServer::with_memory(config.clone(), memory).unwrap();
            let quic = crate::runtime::Quic::bind(&server, tls.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
            let crate::runtime::Quic::Shards(shards) = &quic else {
                panic!("forty workers served HTTP/3 from one endpoint");
            };
            assert_eq!(shards.len(), 16, "twenty shards wanted");
            (server, quic)
        };
        let (measured, _quic) = bind(1 << 40);
        let (server, _quic) = bind(floors + measured.memory.reserved.load(Ordering::Relaxed));
        assert!(!server.memory.under_pressure());
        assert!(server.memory.has_headroom());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_buffer_budget_caps_quic_shards() {
        use super::*;
        let config = Config {
            max_connections: 4,
            max_connections_per_client: 4,
            ..Config::default()
        }
        .validated()
        .unwrap();
        let floors = 4 * (connection_floor(0) + noq_floor(&config.limits).unwrap()) + DOWNLOAD_BLOCK_BYTES;
        let (tls, _) = tls();
        let runtimes: Vec<_> = (0..4)
            .map(|_| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
            })
            .collect();
        let handles: Vec<_> = runtimes.iter().map(|runtime| runtime.handle().clone()).collect();
        let shards = |memory, runtimes: &[tokio::runtime::Handle]| {
            let server = HttpServer::with_memory(config.clone(), memory).unwrap();
            let shards = server.quic_shards(tls.clone(), "127.0.0.1:0".parse().unwrap(), runtimes);
            let count = shards.unwrap().map_or(0, |endpoints| endpoints.len());
            (count, server.memory.reserved.load(Ordering::Relaxed))
        };
        // Each socket keeps its part of the buffers all four offered runtimes would share, however many fit.
        let (four, bytes) = shards(1 << 40, &handles);
        assert_eq!(four, 4);
        assert_eq!(shards(floors + bytes, &handles), (4, bytes));
        assert_eq!(shards(floors + bytes - 1, &handles).0, 3, "three of four shards fit");
        assert_eq!(shards(floors + bytes / 4, &handles).0, 0, "fewer than two shards fit");
    }

    #[tokio::test]
    async fn mixed_transport_exhaustion_preserves_existing_connections() {
        use super::*;
        use rustls::pki_types::ServerName;
        use tokio::net::TcpStream;
        use tokio_rustls::TlsConnector;

        tokio::time::timeout(Duration::from_secs(10), async {
            let (mut tls, mut client_tls) =
                crate::test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"h2"]).unwrap();
            let config = Config {
                max_connections_per_client: 128,
                ..Config::default()
            };
            let floor = connection_floor(0) + noq_floor(&config.limits).unwrap();
            let server = HttpServer::with_memory(config.validated().unwrap(), 8 * (H2_FLOOR_BYTES + floor)).unwrap();
            let server = Arc::new(server);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let link = crate::test_link::Link::tcp(listener.local_addr().unwrap(), Duration::ZERO)
                .await
                .unwrap();
            link.inject(crate::test_link::Fault::None);
            let address = link.address;
            let (stop_h2, stopped_h2) = tokio::sync::oneshot::channel();
            let serving = server
                .clone()
                .serve(NativeKind::H2, listener, Some(Arc::new(tls.clone())), async {
                    let _ = stopped_h2.await;
                });
            let h2_server = tokio::spawn(serving);
            let available = server.memory.available();
            link.inject(crate::test_link::Fault::Stall);
            let pending = TcpStream::connect(address).await.unwrap();
            while server.memory.available() == available {
                tokio::task::yield_now().await;
            }
            assert_eq!(server.memory.available(), available - H2_FLOOR_BYTES);
            drop(pending);
            link.inject(crate::test_link::Fault::Reset);
            while server.memory.available() != available {
                tokio::task::yield_now().await;
            }
            link.inject(crate::test_link::Fault::None);
            tls.alpn_protocols = vec![b"h3".to_vec()];
            let config = server.quic_config(Arc::new(tls), 1).unwrap();
            let endpoint = QuicEndpoint {
                endpoint: noq::Endpoint::server(config.clone(), "127.0.0.1:0".parse().unwrap()).unwrap(),
                config,
                clients: server.client_credit.clone(),
            };
            let quic_address = endpoint.local_addr().unwrap();
            let (stop_h3, stopped_h3) = tokio::sync::oneshot::channel();
            let h3_server = tokio::spawn(server.clone().serve_quic(endpoint, async {
                let _ = stopped_h3.await;
            }));
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
            let client_endpoint = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
            client_endpoint.set_default_client_config(noq::ClientConfig::new(Arc::new(
                noq::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap(),
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
