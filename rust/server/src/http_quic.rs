//! A QUIC connection owns its request futures, session registry, and reset work.

use super::*;
use crate::{webtransport, webtransport_send::ResetQueue};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicUsize;
use tokio::sync::mpsc;
use webtransport::{Incoming, ReceiveStream, TransportError};

const MAX_REQUESTS: usize = 44;
const MAX_PENDING_STREAMS: usize = 64;
const SESSION_QUEUE: usize = 32;
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const WT_SESSION_GONE: u64 = 0x170d7b68;
const MIN_SEND_WINDOW: u64 = 2 * 1024 * 1024;
const MAX_SEND_WINDOW: u64 = 32 * 1024 * 1024;
const SEND_WINDOW_STEP: u64 = 256 * 1024;
const SEND_WINDOW_SHRINK_DELAY: Duration = Duration::from_secs(1);
const BUFFER_BYTES: u32 = 88 * 1024 * 1024;
type Work = Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send>>;

impl HttpServer {
    pub fn quic_config(
        &self,
        tls: Arc<rustls::ServerConfig>,
    ) -> Result<quinn::ServerConfig, ConfigError> {
        if tls.alpn_protocols != [b"h3".to_vec()] {
            return Err("HTTP/3 listener requires h3-only TLS ALPN".into());
        }
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(
            u32::try_from(
                self.config.limits.operations_per_client
                    + self.config.limits.sessions_per_client
                    + 4,
            )?
            .into(),
        );
        transport.max_concurrent_uni_streams(23_u32.into());
        // Noq uses fixed receive credit rather than quic-go's autotuning.
        // A 1 MiB stream window capped one upload near 80 Mbit/s at 100 ms
        // RTT. These 8/16 MiB limits bound unconsumed inbound data per
        // stream/connection; connection admission bounds their aggregate.
        transport.stream_receive_window((8 * 1024 * 1024_u32).into());
        transport.receive_window((16 * 1024 * 1024_u32).into());
        transport.send_window(MIN_SEND_WINDOW);
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
        let mut reclaim = tokio::time::interval(Duration::from_millis(250));
        reclaim.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                Some(_) = connections.join_next() => {}
                _ = reclaim.tick() => self.memory.reclaim(),
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
                    let Some(lease) = self.memory.acquire(BUFFER_BYTES) else {
                        incoming.refuse();
                        continue;
                    };
                    let Ok(connecting) = incoming.accept() else { continue; };
                    let Some(weak) = connecting.weak_handle() else { continue; };
                    let window = Arc::new(Mutex::new(SendWindow::new()));
                    self.memory.quic.lock().expect("memory registry poisoned").push(QuicReservation {
                        weak, _lease: lease, window: window.clone(),
                    });
                    let server = self.clone();
                    connections.spawn(async move {
                        let _permit = permit;
                        if let Ok(Ok(quic)) = tokio::time::timeout(Duration::from_secs(5), connecting).await {
                            let _ = server.serve_quic_connection(quic, peer, window).await;
                        }
                    });
                }
            }
        };
        endpoint.close(0_u32.into(), b"server stopped");
        connections.shutdown().await;
        // UDP draining is bounded; an unresponsive peer cannot delay shutdown.
        let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
        result
    }

    async fn serve_quic_connection(
        self: Arc<Self>,
        quic: quinn::Connection,
        peer: SocketAddr,
        window: Arc<Mutex<SendWindow>>,
    ) -> Result<(), TransportError> {
        let (resets, mut pending_resets) = ResetQueue::new(MAX_REQUESTS);
        let mut initializing = CloseOnDrop(Some(quic.clone()));
        let http = tokio::time::timeout(
            HEADER_TIMEOUT,
            webtransport::Connection::new(quic.clone(), 1),
        )
        .await??;
        let mut connection = OwnedConnection {
            http,
            quic,
            requests: FuturesUnordered::new(),
            cleanup: FuturesUnordered::new(),
            sessions: Sessions::default(),
        };
        let active_responses = Arc::new(AtomicUsize::new(0));
        initializing.0.take();
        let mut expiry = tokio::time::interval(Duration::from_secs(1));
        expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tuning = tokio::time::interval(Duration::from_millis(250));
        tuning.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = expiry.tick() => connection.sessions.expire(),
                _ = tuning.tick() => {
                    let mut window = window.lock().expect("send window poisoned");
                    if connection.requests.is_empty() {
                        window.release(&connection.quic);
                    } else {
                        window.update(&connection.quic, &self.memory.bytes);
                    }
                }
                Some(_) = connection.requests.next() => {},
                Some(_) = connection.cleanup.next() => {},
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
                            if connection.requests.len() >= MAX_REQUESTS {
                                drop(request);
                                continue;
                            }
                            let server = self.clone();
                            let sessions = connection.sessions.clone();
                            let quic = connection.quic.clone();
                            let resets = resets.clone();
                            let active_responses = active_responses.clone();
                            connection.requests.push(Box::pin(async move {
                                let (request, stream) = tokio::time::timeout(HEADER_TIMEOUT, request.resolve_request()).await??;
                                if request.method() == Method::CONNECT {
                                    server.serve_webtransport(request, stream, quic, peer, resets, sessions).await
                                } else {
                                    server.serve_http3_request(request, stream, peer, active_responses).await.map_err(Into::into)
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

pub(super) struct MemoryBudget {
    bytes: Arc<tokio::sync::Semaphore>,
    retry_available: usize,
    quic: Mutex<Vec<QuicReservation>>,
}

struct QuicReservation {
    weak: quinn::WeakConnectionHandle,
    _lease: tokio::sync::OwnedSemaphorePermit,
    window: Arc<Mutex<SendWindow>>,
}

impl MemoryBudget {
    pub(super) fn new(bytes: usize) -> Self {
        Self {
            bytes: Arc::new(tokio::sync::Semaphore::new(bytes)),
            retry_available: bytes - bytes / 4,
            quic: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn acquire(&self, bytes: u32) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.reclaim();
        self.bytes.clone().try_acquire_many_owned(bytes).ok()
    }

    fn under_pressure(&self) -> bool {
        self.bytes.available_permits() <= self.retry_available
    }

    fn reclaim(&self) {
        self.quic
            .lock()
            .expect("memory registry poisoned")
            .retain(|reservation| {
                if let Some(connection) = reservation.weak.upgrade() {
                    reservation
                        .window
                        .lock()
                        .expect("send window poisoned")
                        .refund(&connection);
                    true
                } else {
                    false
                }
            });
    }
}

struct SendWindow {
    limit: u64,
    last: Option<(tokio::time::Instant, u64)>,
    low_demand_since: Option<tokio::time::Instant>,
    extra: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl SendWindow {
    fn new() -> Self {
        Self {
            limit: MIN_SEND_WINDOW,
            last: None,
            low_demand_since: None,
            extra: None,
        }
    }

    fn current(&self) -> u64 {
        self.limit
    }

    fn reserved(&self) -> u64 {
        MIN_SEND_WINDOW
            + self
                .extra
                .as_ref()
                .map_or(0, |extra| extra.num_permits() as u64)
    }

    fn refund(&mut self, connection: &quinn::Connection) {
        if let Some(extra) = &mut self.extra {
            let retained = self.limit.max(connection.send_buffered_bytes());
            let reserved = MIN_SEND_WINDOW + extra.num_permits() as u64;
            drop(extra.split(reserved.saturating_sub(retained) as usize));
            if extra.num_permits() == 0 {
                self.extra = None;
            }
        }
    }

    fn update(&mut self, connection: &quinn::Connection, budget: &Arc<tokio::sync::Semaphore>) {
        self.refund(connection);
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
            // grow without reserving the full window on fast local links.
            let target = desired_send_window(
                sent.saturating_sub(previous),
                path.rtt,
                now.duration_since(last),
            );
            if target == MIN_SEND_WINDOW && self.extra.is_some() {
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

    fn grow(&mut self, target: u64, budget: &Arc<tokio::sync::Semaphore>) -> Option<u64> {
        let target = target.min(MAX_SEND_WINDOW);
        if target <= self.limit {
            return None;
        }
        let wanted = target.saturating_sub(self.reserved());
        let granted = wanted.min(budget.available_permits() as u64);
        if wanted > 0 && granted < SEND_WINDOW_STEP {
            return None;
        }
        if granted > 0 {
            let permit = budget.clone().try_acquire_many_owned(granted as u32).ok()?;
            match &mut self.extra {
                Some(extra) => extra.merge(permit),
                None => self.extra = Some(permit),
            }
        }
        self.limit = target.min(self.reserved());
        Some(self.limit)
    }

    fn release(&mut self, connection: &quinn::Connection) {
        self.last = None;
        self.low_demand_since = None;
        if self.current() != MIN_SEND_WINDOW {
            self.limit = MIN_SEND_WINDOW;
            connection.set_send_window(self.current());
        }
        self.refund(connection);
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
    requests: FuturesUnordered<Work>,
    cleanup: FuturesUnordered<Work>,
    sessions: Sessions,
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
        self.quic.close(0_u32.into(), b"connection ended");
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
            registry
                .pending
                .push_back((tokio::time::Instant::now(), id, stream));
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
            let (_, _, stream) = registry
                .pending
                .pop_front()
                .expect("checked pending stream");
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

    #[test]
    fn send_window_growth_shares_one_budget_and_returns_it_on_drop() {
        let budget = Arc::new(tokio::sync::Semaphore::new(40 << 20));
        let (mut first, mut second) = (SendWindow::new(), SendWindow::new());
        assert_eq!(first.grow(MAX_SEND_WINDOW, &budget), Some(MAX_SEND_WINDOW));
        assert_eq!(
            second.grow(MAX_SEND_WINDOW, &budget),
            Some(MIN_SEND_WINDOW + (10 << 20))
        );
        assert_eq!(second.grow(MAX_SEND_WINDOW, &budget), None);
        drop(first);
        assert_eq!(second.grow(MAX_SEND_WINDOW, &budget), Some(MAX_SEND_WINDOW));
        assert_eq!(budget.available_permits(), 10 << 20);
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
    async fn idle_send_window_returns_budget_while_control_stream_stays_open() {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

        let (certificate, key) = crate::test_identity::generate_identity("localhost").unwrap();
        let certificate = CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap();
        let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
        let config = quinn::ServerConfig::with_single_cert(vec![certificate.clone()], key).unwrap();
        let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).unwrap();
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut config = quinn::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
        let mut transport = quinn::TransportConfig::default();
        transport.stream_receive_window((8 * 1024 * 1024_u32).into());
        transport.receive_window((16 * 1024 * 1024_u32).into());
        config.transport_config(Arc::new(transport));
        client.set_default_client_config(config);
        let attempt = client
            .connect(server.local_addr().unwrap(), "localhost")
            .unwrap();
        let connecting = tokio::time::timeout(Duration::from_secs(1), server.accept())
            .await
            .unwrap()
            .unwrap()
            .accept()
            .unwrap();
        let memory = super::MemoryBudget::new(super::BUFFER_BYTES as usize);
        let lease = memory.acquire(super::BUFFER_BYTES).unwrap();
        memory.quic.lock().unwrap().push(super::QuicReservation {
            weak: connecting.weak_handle().unwrap(),
            _lease: lease,
            window: Arc::new(std::sync::Mutex::new(SendWindow::new())),
        });
        drop(connecting);
        drop(attempt);
        assert!(
            memory.acquire(1).is_none(),
            "cancelled handshake refunded before transport destruction"
        );
        tokio::time::pause();
        for _ in 0..90 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            memory.reclaim();
            if memory.bytes.available_permits() == super::BUFFER_BYTES as usize {
                break;
            }
        }
        tokio::time::resume();
        assert_eq!(
            memory.bytes.available_permits(),
            super::BUFFER_BYTES as usize
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            let (client, server) = tokio::join!(
                client
                    .connect(server.local_addr().unwrap(), "localhost")
                    .unwrap(),
                async { server.accept().await.unwrap().await.unwrap() },
            );
            let client = client.unwrap();
            let (mut request, mut response) = client.open_bi().await.unwrap();
            request.write_all(b"ping").await.unwrap();
            let (mut replies, mut requests) = server.accept_bi().await.unwrap();
            let mut ping = [0; 4];
            requests.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");

            let budget = Arc::new(tokio::sync::Semaphore::new(
                (MAX_SEND_WINDOW - MIN_SEND_WINDOW) as usize,
            ));
            let mut window = SendWindow::new();
            server.set_send_window(window.grow(MAX_SEND_WINDOW, &budget).unwrap());
            assert_eq!(budget.available_permits(), 0);
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
                        budget.available_permits(),
                        0,
                        "one quiet sample must not shrink an active window"
                    );
                }
            }
            assert_eq!(window.current(), MIN_SEND_WINDOW);
            let mut next = SendWindow::new();
            assert_eq!(next.grow(MAX_SEND_WINDOW, &budget), Some(MAX_SEND_WINDOW));
            drop(next);
            request.write_all(b"live").await.unwrap();
            requests.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"live");
            server.set_send_window(window.grow(MAX_SEND_WINDOW, &budget).unwrap());
            let mut payload = server.open_uni().await.unwrap();
            payload.write_all(&vec![1; 4 * 1024 * 1024]).await.unwrap();
            let outstanding = server.send_buffered_bytes();
            assert!(outstanding > MIN_SEND_WINDOW);
            window.release(&server);
            assert_eq!(window.reserved(), outstanding);
            let retained = outstanding - MIN_SEND_WINDOW;
            assert_eq!(
                budget.available_permits() as u64,
                MAX_SEND_WINDOW - MIN_SEND_WINDOW - retained
            );
            let memory = super::MemoryBudget::new(super::BUFFER_BYTES as usize);
            let lease = memory.acquire(super::BUFFER_BYTES).unwrap();
            let held_window = Arc::new(std::sync::Mutex::new(window));
            memory.quic.lock().unwrap().push(super::QuicReservation {
                weak: server.weak_handle(),
                _lease: lease,
                window: held_window,
            });
            server.close(0_u32.into(), b"closed with owned stream");
            drop(server);
            memory.reclaim();
            assert!(
                memory.acquire(1).is_none(),
                "closed stream refunded the connection reservation"
            );
            drop(payload);
            drop(replies);
            drop(requests);
            client.close(0_u32.into(), b"done");
            tokio::time::pause();
            for _ in 0..20 {
                tokio::time::advance(Duration::from_millis(100)).await;
                tokio::task::yield_now().await;
                memory.reclaim();
                if memory.bytes.available_permits() == super::BUFFER_BYTES as usize {
                    break;
                }
            }
            tokio::time::resume();
            assert_eq!(
                memory.bytes.available_permits(),
                super::BUFFER_BYTES as usize
            );
            client.close(0_u32.into(), b"done");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn mixed_transport_exhaustion_preserves_existing_connections() {
        exercise_memory(false).await;
    }

    #[tokio::test]
    async fn sixteen_client_stage_transition_keeps_draining_reservations() {
        exercise_memory(true).await;
    }

    async fn exercise_memory(topology: bool) {
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
            let server = HttpServer::with_memory(
                Arc::new(Config {
                    max_connections_per_client: 128,
                    trusted_proxies: if topology {
                        vec!["127.0.0.1/32".parse().unwrap()]
                    } else {
                        Vec::new()
                    },
                    ..Config::default()
                }),
                if topology {
                    8 * 1024 * 1024 * 1024 - 32 * (MAX_SEND_WINDOW - MIN_SEND_WINDOW) as usize
                } else {
                    2 * BUFFER_BYTES as usize
                        + http_h2::BUFFER_BYTES as usize
                        + DOWNLOAD_BLOCK_BYTES
                },
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
            let h2_server = tokio::spawn(server.clone().serve_http2(
                listener,
                Arc::new(tls.clone()),
                async {
                    let _ = stopped_h2.await;
                },
            ));
            if topology {
                let available = server.memory.bytes.available_permits();
                link.inject(crate::test_link::Fault::Stall);
                let pending = TcpStream::connect(address).await.unwrap();
                while server.memory.bytes.available_permits() == available {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    server.memory.bytes.available_permits(),
                    available - http_h2::BUFFER_BYTES as usize
                );
                drop(pending);
                link.inject(crate::test_link::Fault::Reset);
                while server.memory.bytes.available_permits() != available {
                    tokio::task::yield_now().await;
                }
                link.inject(crate::test_link::Fault::None);
            }
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
            if topology {
                let mut h2_clients = Vec::new();
                let mut h2_drivers = JoinSet::new();
                for _ in 0..15 {
                    let stream = connector
                        .connect(
                            ServerName::try_from("localhost").unwrap(),
                            TcpStream::connect(address).await.unwrap(),
                        )
                        .await
                        .unwrap();
                    let (client, connection) = h2::client::handshake(stream).await.unwrap();
                    h2_clients.push(client);
                    h2_drivers.spawn(connection);
                }
                let link = crate::test_link::Link::udp(quic_address, Duration::from_millis(150))
                    .await
                    .unwrap();
                let mut clients = Vec::new();
                let mut opening = FuturesUnordered::new();
                for _ in 0..48 {
                    opening.push(client_endpoint.connect(link.address, "localhost").unwrap());
                }
                while let Some(connection) = opening.next().await {
                    clients.push(connection.unwrap());
                }
                let held: Vec<_> = {
                    let registry = server.memory.quic.lock().unwrap();
                    assert_eq!(registry.len(), 48);
                    registry
                        .iter()
                        .take(16)
                        .map(|reservation| reservation.weak.upgrade().unwrap())
                        .collect()
                };
                for connection in clients.drain(..16) {
                    connection.close(0_u32.into(), b"next stage");
                }
                for _ in 0..16 {
                    opening.push(client_endpoint.connect(link.address, "localhost").unwrap());
                }
                while let Some(connection) = opening.next().await {
                    clients.push(connection.unwrap());
                }
                assert_eq!(server.memory.quic.lock().unwrap().len(), 64);
                assert_eq!(
                    server.memory.bytes.available_permits(),
                    1024 * 1024 * 1024 - DOWNLOAD_BLOCK_BYTES
                );
                drop(held);
                drop(clients);
                client_endpoint.close(0_u32.into(), b"done");
                stop_h2.send(()).unwrap();
                stop_h3.send(()).unwrap();
                h2_server.await.unwrap().unwrap();
                h3_server.await.unwrap().unwrap();
                h2_drivers.shutdown().await;
                drop(h2_clients);
                h2_driver.abort();
                let _ = h2_driver.await;
                return;
            }
            let unloaded = crate::test_link::Link::udp(quic_address, Duration::from_millis(50))
                .await
                .unwrap();
            let started = tokio::time::Instant::now();
            let quic = client_endpoint
                .connect(unloaded.address, "localhost")
                .unwrap()
                .await
                .unwrap();
            assert!(
                started.elapsed() < Duration::from_millis(150),
                "Retry below a quarter of the memory budget"
            );
            let (mut driver, mut h3) = h3::client::new(h3_noq::Connection::new(quic))
                .await
                .unwrap();
            let h3_driver = tokio::spawn(async move { driver.wait_idle().await });
            let second = client_endpoint
                .connect(quic_address, "localhost")
                .unwrap()
                .await
                .unwrap();
            assert_eq!(server.memory.bytes.available_permits(), 0);
            let pressured = crate::test_link::Link::udp(quic_address, Duration::from_millis(50))
                .await
                .unwrap();
            let started = tokio::time::Instant::now();
            assert!(
                client_endpoint
                    .connect(pressured.address, "localhost")
                    .unwrap()
                    .await
                    .is_err()
            );
            assert!(
                started.elapsed() >= Duration::from_millis(180),
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
                response
                    .body_mut()
                    .flow_control()
                    .release_capacity(data.len())
                    .unwrap();
            }
            assert_eq!(bytes, 4);
            let mut stream = h3.send_request(request()).await.unwrap();
            stream.finish().await.unwrap();
            assert_eq!(
                stream.recv_response().await.unwrap().status(),
                StatusCode::OK
            );
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
            drop(stream);
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
        client.set_default_client_config(
            quinn::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap(),
        );
        let (sender, receiver) = tokio::join!(
            client
                .connect(server.local_addr().unwrap(), "localhost")
                .unwrap(),
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
        let recv = poll_fn(|cx| {
            <h3_noq::Connection as h3::quic::Connection<Bytes>>::poll_accept_recv(&mut adapter, cx)
        })
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
