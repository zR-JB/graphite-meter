//! A QUIC connection owns its request futures, session registry, and reset work.

use super::*;
use crate::{webtransport, webtransport_send::ResetQueue};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicUsize;
use tokio::sync::mpsc;
use webtransport::{Incoming, ReceiveStream, TransportError};

const MAX_REQUESTS: usize = 256;
const MAX_PENDING_STREAMS: usize = 64;
const SESSION_QUEUE: usize = 32;
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const WT_SESSION_GONE: u64 = 0x170d7b68;
const MIN_SEND_WINDOW: u64 = 2 * 1024 * 1024;
const MAX_SEND_WINDOW: u64 = 32 * 1024 * 1024;
const SEND_WINDOW_STEP: u64 = 256 * 1024;
pub(super) const SEND_WINDOW_BUDGET: usize = 256 * 1024 * 1024;
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
        transport.max_concurrent_bidi_streams((MAX_REQUESTS as u32).into());
        transport.max_concurrent_uni_streams(260_u32.into());
        // Noq uses fixed receive credit rather than quic-go's autotuning.
        // A 1 MiB stream window capped one upload near 80 Mbit/s at 100 ms
        // RTT. These 8/16 MiB limits bound unconsumed inbound data per
        // stream/connection; connection admission bounds their aggregate.
        transport.stream_receive_window((8 * 1024 * 1024_u32).into());
        transport.receive_window((16 * 1024 * 1024_u32).into());
        transport.send_window(MIN_SEND_WINDOW);
        transport.datagram_receive_buffer_size(Some(64 * 1024));
        transport.datagram_send_buffer_size(64 * 1024);
        transport.max_idle_timeout(Some(Duration::from_secs(60).try_into()?));
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
                        && self.connections.stats().active >= self.config.max_connections / 4
                    {
                        let _ = incoming.retry();
                        continue;
                    }
                    let peer = incoming.remote_address();
                    let Ok(permit) = self.connections.acquire(peer) else {
                        incoming.refuse();
                        continue;
                    };
                    let server = self.clone();
                    connections.spawn(async move {
                        let _permit = permit;
                        if let Ok(Ok(quic)) = tokio::time::timeout(HEADER_TIMEOUT, incoming).await {
                            let _ = server.serve_quic_connection(quic, peer).await;
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
        let mut window = SendWindow::new();
        loop {
            tokio::select! {
                _ = expiry.tick() => connection.sessions.expire(),
                _ = tuning.tick(), if !connection.requests.is_empty() || window.extra.is_some() => {
                    if connection.requests.is_empty() {
                        window.release(&connection.quic);
                    } else {
                        window.update(&connection.quic, &self.quic_send_budget);
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

struct SendWindow {
    last: Option<(tokio::time::Instant, u64)>,
    extra: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl SendWindow {
    fn new() -> Self {
        Self {
            last: None,
            extra: None,
        }
    }

    fn current(&self) -> u64 {
        MIN_SEND_WINDOW
            + self
                .extra
                .as_ref()
                .map_or(0, |extra| extra.num_permits() as u64)
    }

    fn update(&mut self, connection: &quinn::Connection, budget: &Arc<tokio::sync::Semaphore>) {
        // Read the aggregate first so a concurrent send on the initial path
        // cannot look like traffic on another path.
        let all_sent = connection.stats().udp_tx.bytes;
        let Some(path) = connection.path_stats(quinn::PathId::ZERO) else {
            self.last = None;
            return;
        };
        // A second path invalidates this path's throughput estimate.
        if all_sent > path.udp_tx.bytes {
            self.last = None;
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
            if let Some(window) = self.grow(target, budget) {
                connection.set_send_window(window);
            }
        }
        self.last = Some((now, sent));
    }

    fn grow(&mut self, target: u64, budget: &Arc<tokio::sync::Semaphore>) -> Option<u64> {
        let wanted = target.min(MAX_SEND_WINDOW).saturating_sub(self.current());
        let granted = wanted.min(budget.available_permits() as u64);
        if granted < SEND_WINDOW_STEP {
            return None;
        }
        let permit = budget.clone().try_acquire_many_owned(granted as u32).ok()?;
        match &mut self.extra {
            Some(extra) => extra.merge(permit),
            None => self.extra = Some(permit),
        }
        Some(self.current())
    }

    fn release(&mut self, connection: &quinn::Connection) {
        self.last = None;
        if self.extra.is_some() {
            connection.set_send_window(MIN_SEND_WINDOW);
            self.extra = None;
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
        let Ok(size) = u32::try_from(payload.len().max(1)) else {
            return;
        };
        let Ok(budget) = self.datagram_bytes.clone().try_acquire_many_owned(size) else {
            return;
        };
        let registry = self.registry.lock().expect("session registry poisoned");
        if let Some(sender) = registry.active.get(&id) {
            // Loss is permitted; a slow session never blocks connection control.
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
