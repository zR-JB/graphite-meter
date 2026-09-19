//! A QUIC connection owns its request futures, session registry, and reset work.

use super::http_wt::SessionEvent;
use super::*;
use crate::{webtransport, webtransport_send::ResetQueue};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;
use webtransport::{Incoming, ReceiveStream, TransportError};

const MAX_REQUESTS: usize = 256;
const MAX_PENDING_STREAMS: usize = 64;
const SESSION_QUEUE: usize = 32;
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const WT_SESSION_GONE: u64 = 0x170d7b68;
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
        transport.stream_receive_window((1024 * 1024_u32).into());
        transport.receive_window((4 * 1024 * 1024_u32).into());
        transport.send_window(4 * 1024 * 1024);
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
                Some(done) = connections.join_next() => {
                    if let Err(error) = done {
                        break Err(error.into());
                    }
                }
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break Ok(()); };
                    // Validate address ownership before reserving scarce shared
                    // connection capacity or performing a TLS handshake. Retry
                    // is stateless and stays within QUIC's amplification bound.
                    if !incoming.remote_address_validated() {
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
            webtransport::Connection::new(quic.clone(), self.config.limits.sessions as u64),
        )
        .await??;
        let mut connection = OwnedConnection {
            http,
            quic,
            requests: FuturesUnordered::new(),
            cleanup: FuturesUnordered::new(),
            sessions: Sessions::default(),
        };
        initializing.0.take();
        let mut expiry = tokio::time::interval(Duration::from_secs(1));
        expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = expiry.tick() => connection.sessions.expire(),
                Some(_) = connection.requests.next() => {},
                Some(result) = connection.cleanup.next() => result?,
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
                            connection.requests.push(Box::pin(async move {
                                let (request, stream) = tokio::time::timeout(HEADER_TIMEOUT, request.resolve_request()).await??;
                                if request.method() == Method::CONNECT {
                                    let id = stream.send_id().into_inner();
                                    let (_registration, events) = sessions.register(id);
                                    server.serve_webtransport(request, stream, quic, peer, resets, events).await
                                } else {
                                    server.serve_http3_request(request, stream, peer).await.map_err(Into::into)
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

#[derive(Default)]
struct Registry {
    active: HashMap<u64, mpsc::Sender<SessionEvent>>,
    pending: VecDeque<(tokio::time::Instant, u64, ReceiveStream)>,
}

#[derive(Clone)]
struct Sessions {
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
    fn register(&self, id: u64) -> (Registration, mpsc::Receiver<SessionEvent>) {
        let (sender, receiver) = mpsc::channel(SESSION_QUEUE);
        let mut registry = self.registry.lock().expect("session registry poisoned");
        registry.active.insert(id, sender.clone());
        let mut remaining = VecDeque::new();
        while let Some((started, target, stream)) = registry.pending.pop_front() {
            if target == id {
                deliver(&sender, SessionEvent::Stream(stream));
            } else {
                remaining.push_back((started, target, stream));
            }
        }
        registry.pending = remaining;
        (
            Registration {
                sessions: self.clone(),
                id,
            },
            receiver,
        )
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
            deliver(
                sender,
                SessionEvent::Datagram {
                    payload,
                    _budget: budget,
                },
            );
        }
    }

    fn stream(&self, id: u64, stream: ReceiveStream) {
        let mut registry = self.registry.lock().expect("session registry poisoned");
        if let Some(sender) = registry.active.get(&id) {
            deliver(sender, SessionEvent::Stream(stream));
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

struct Registration {
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

fn deliver(sender: &mpsc::Sender<SessionEvent>, event: SessionEvent) {
    if let Err(error) = sender.try_send(event)
        && let SessionEvent::Stream(stream) = error.into_inner()
    {
        stop(stream);
    }
}

fn stop(mut stream: ReceiveStream) {
    use h3::quic::RecvStream;
    stream.stop_sending(WT_SESSION_GONE);
}
