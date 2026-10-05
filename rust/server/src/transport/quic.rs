//! HTTP/3 over QUIC: the endpoint with its buffers held at bind, Retry for unvalidated handshakes under pressure, each
//! connection's floor, and the connection driver over the HTTP/3 layer.

mod budget;
mod request;
mod window;

pub use budget::{endpoint_bytes, floor_bytes, noq_floor};

use super::{
    PEER_FAILURES,
    accept::Listen,
    lifecycle::{Event, Grace, Lifecycle},
    tls::Certificates,
};
use crate::{
    app::{App, Connection, Endpoint},
    config::{Limits, path_error},
    lane::Work,
    limits::Lease,
    log,
};
use budget::{ConnectionBudget, INCOMING_BYTES, INCOMING_TOTAL_BYTES};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_http3::{self as http3, server};
use graphite_meter_proto::{lane::LaneEnding, text::quote};
use noq::{AsyncUdpSocket, UdpSender, crypto::rustls::QuicServerConfig};
use request::Requests;
use std::{
    future::{Future, poll_fn},
    io,
    net::SocketAddr,
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    runtime::Handle,
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_util::sync::CancellationToken;
use window::{SendWindow, Window};

/// A whole handshake has this long, as quic-go's.
const HANDSHAKE_BOUND: Duration = Duration::from_secs(10);
/// How often a connection's send window follows its demand.
const SEND_WINDOW_TUNING: Duration = Duration::from_millis(250);
/// Connections closed once the shutdown drain ended have this long to send their close.
const CLOSE_FLUSH: Duration = Duration::from_secs(1);

/// The endpoint's accepts; dropping it refuses new connections while running ones go on.
pub struct Listener {
    endpoint: noq::Endpoint,
    app: Arc<App>,
    runtime: Handle,
}

/// What the endpoint's connections share.
#[derive(Clone)]
pub struct Http3 {
    app: Arc<App>,
    config: noq::ServerConfig,
    certificates: Arc<Certificates>,
    max_requests: usize,
    shutdown: CancellationToken,
}

/// Binds an endpoint on `address` that runs with its connections on `runtime` and holds its buffers from the budget
/// while it runs; `covered` refuses buffer bytes the budget does not cover beside every connection's floor.
pub fn bind(
    address: SocketAddr,
    runtime: Handle,
    app: &Arc<App>,
    limits: &Limits,
    certificates: &Arc<Certificates>,
    covered: impl FnOnce(usize) -> Result<(), String>,
    shutdown: &CancellationToken,
) -> Result<(Listener, Http3), String> {
    let _entered = runtime.enter();
    let bound = graphite_meter_net::bind_udp(address, 1);
    let (socket, warning) = bound.map_err(|error| path_error("listen udp", &address.to_string(), &error))?;
    if let Some(warning) = warning {
        log!("{warning}");
    }
    let buffers = socket2::SockRef::from(&socket);
    let kernel = buffers
        .recv_buffer_size()
        .and_then(|receive| Ok(receive + buffers.send_buffer_size()?));
    let kernel = kernel.map_err(|error| error.to_string())?;
    let quic_runtime = noq::default_runtime().ok_or("no async runtime for QUIC")?;
    let socket = quic_runtime
        .wrap_udp_socket(socket)
        .map_err(|error| error.to_string())?;
    let endpoint_config = noq::EndpointConfig::default();
    let segments = socket.max_receive_segments().get();
    let bytes = endpoint_bytes(&endpoint_config, limits.connections, kernel, segments)
        .ok_or("QUIC endpoint buffer size overflow")?;
    covered(bytes)?;
    let lease = app
        .budget()
        .reserve(bytes)
        .ok_or("the buffer budget cannot cover the QUIC endpoint buffers")?;
    let socket = Box::new(Budgeted { socket, lease: Arc::new(lease) });
    let config = server_config(app, limits, certificates)?;
    let endpoint = noq::Endpoint::new_with_abstract_socket(endpoint_config, Some(config.clone()), socket, quic_runtime);
    let endpoint = endpoint.map_err(|error| error.to_string())?;
    let max_requests = budget::max_requests(limits) as usize;
    let listener = Listener { endpoint, app: app.clone(), runtime };
    let certificates = certificates.clone();
    let shutdown = shutdown.clone();
    Ok((
        listener,
        Http3 {
            app: app.clone(),
            config,
            certificates,
            max_requests,
            shutdown,
        },
    ))
}

/// Admits the connections and queued handshake packets the limits allow, with the transport whose floor the budget
/// counts.
fn server_config(app: &App, limits: &Limits, certificates: &Arc<Certificates>) -> Result<noq::ServerConfig, String> {
    let crypto = QuicServerConfig::try_from(certificates.server_config(b"h3")).map_err(|error| error.to_string())?;
    let mut config = noq::ServerConfig::with_crypto(Arc::new(crypto));
    config
        .max_incoming(limits.connections)
        .incoming_buffer_size(INCOMING_BYTES)
        .incoming_buffer_size_total(INCOMING_TOTAL_BYTES);
    let mut transport = budget::transport(limits);
    transport.shared_budget(Some(app.budget().noq()));
    config.transport_config(Arc::new(transport));
    Ok(config)
}

impl Listener {
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// The runtime the endpoint and its connections run on.
    pub fn runtime(&self) -> Handle {
        self.runtime.clone()
    }
}

impl Listen for Listener {
    type Connection = noq::Incoming;

    /// The next handshake that needs no Retry: Retry spends a round trip to protect admission under load, and a
    /// source's QUIC share from Initials that may be spoofed.
    async fn accept(&mut self) -> io::Result<(noq::Incoming, SocketAddr)> {
        loop {
            let incoming = self.endpoint.accept().await.ok_or(io::ErrorKind::InvalidInput)?;
            let peer = incoming.remote_address();
            if !incoming.remote_address_validated() && self.app.quic_retry(peer.ip().to_canonical()) {
                let _ = incoming.retry();
                continue;
            }
            return Ok((incoming, peer));
        }
    }

    fn name(&self) -> String {
        self.local_addr()
            .map_or_else(|_| "udp".into(), |address| format!("udp {address}"))
    }

    /// Closes what the drain left, handshakes included, with H3_NO_ERROR, and lets the closes reach the socket.
    fn closer(&self) -> impl Future<Output = ()> + Send + 'static {
        let endpoint = self.endpoint.clone();
        async move {
            endpoint.close(http3::Code::H3_NO_ERROR.into(), b"");
            let _ = timeout(CLOSE_FLUSH, endpoint.wait_all_draining()).await;
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.endpoint.set_server_config(None);
    }
}

impl Http3 {
    /// An incoming connection's work, holding its floor from now; one whose floor does not fit is refused.
    pub fn connection(&self, incoming: noq::Incoming, peer: SocketAddr) -> impl Future<Output = ()> + Send + use<> {
        let floor = floor_bytes(self.certificates.handshake_bytes());
        let budget = self.app.budget().lease(floor).map(|floor| self.budget(floor));
        let listener = self.clone();
        async move {
            let Some((budget, config)) = budget else {
                return incoming.refuse();
            };
            let Ok(connecting) = incoming.accept_with(config) else {
                return;
            };
            let address = peer.ip().to_canonical();
            let quic = tokio::select! {
                biased;
                () = listener.shutdown.cancelled() => return,
                connected = timeout(HANDSHAKE_BOUND, connecting) => match connected {
                    Ok(Ok(quic)) => quic,
                    Ok(Err(error)) => {
                        if !ended_normally(&error.clone().into()) {
                            let error = quote(&error.to_string());
                            PEER_FAILURES.write(format_args!("[gm:h3] QUIC handshake error from {address}: {error}"));
                        }
                        return;
                    }
                    Err(_) => {
                        PEER_FAILURES.write(format_args!("[gm:h3] QUIC handshake error from {address}: timed out"));
                        return;
                    }
                }
            };
            if let Err(error) = listener.serve(quic, budget, peer).await
                && !ended_normally(&error)
            {
                let error = quote(&error.to_string());
                PEER_FAILURES.write(format_args!("[gm:h3] webtransport connection: {error}"));
            }
        }
    }

    /// The connection's share of the budget, holding `floor`, and the configuration that has noq charge it.
    fn budget(&self, floor: Lease) -> (Arc<ConnectionBudget>, Arc<noq::ServerConfig>) {
        let budget = Arc::new(ConnectionBudget::new(self.app.budget(), floor));
        let mut transport = (*self.config.transport).clone();
        transport.shared_budget(Some(budget.clone()));
        let mut config = self.config.clone();
        config.transport_config(Arc::new(transport));
        (budget, Arc::new(config))
    }

    /// Serves requests until the layer ends the connection or its lifecycle closes it; a stop shuts the layer down
    /// with every session's close.
    async fn serve(
        &self,
        quic: noq::Connection,
        budget: Arc<ConnectionBudget>,
        peer: SocketAddr,
    ) -> Result<(), http3::Error> {
        let connection = Connection {
            endpoint: Endpoint::Quic,
            peer: peer.ip(),
            work: Work::default(),
        };
        let window = Arc::new(Window::new(self.app.clone(), quic.clone(), budget.clone()));
        let requests = Requests::new(self.app.clone(), connection.clone(), window.clone());
        let mut http = server::Connection::new(quic.clone(), Some(budget));
        let mut lifecycle = Lifecycle::new(connection.work, Grace::Once);
        let (mut running, mut send_window) = (FuturesUnordered::new(), SendWindow::default());
        let mut tuning = interval(SEND_WINDOW_TUNING);
        tuning.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut stopping = pin!(self.shutdown.cancelled());
        let mut stopped = false;
        loop {
            tokio::select! {
                request = http.next() => match request? {
                    Some(request) if running.len() < self.max_requests => running.push(requests.serve(request)),
                    Some(request) => request.reject(),
                    None => return Ok(()),
                },
                Some(()) = running.next() => {}
                () = &mut stopping, if !stopped => {
                    stopped = true;
                    lifecycle.stop();
                    http.shutdown(LaneEnding::Shutdown.webtransport_code(), LaneEnding::Shutdown.reason());
                }
                // The layer closes connections without a live request itself.
                event = poll_fn(|cx| lifecycle.poll(cx, window.raised(), true)) => match event {
                    Event::GoAway => http.goaway(),
                    Event::Close => return Ok(()),
                },
                _ = tuning.tick() => send_window.tune(&quic, !running.is_empty(), self.app.budget()),
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

/// The endpoint's socket, whose buffers hold their reservation until the socket and its senders drop.
#[derive(Debug)]
struct Budgeted {
    socket: Box<dyn AsyncUdpSocket>,
    lease: Arc<Lease>,
}

impl AsyncUdpSocket for Budgeted {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        let sender = self.socket.create_sender();
        Box::pin(BudgetedSender { sender, _lease: self.lease.clone() })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        buffers: &mut [io::IoSliceMut<'_>],
        meta: &mut [noq::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.socket.poll_recv(cx, buffers, meta)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn max_receive_segments(&self) -> NonZeroUsize {
        self.socket.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.socket.may_fragment()
    }
}

#[derive(Debug)]
struct BudgetedSender {
    sender: Pin<Box<dyn UdpSender>>,
    _lease: Arc<Lease>,
}

impl UdpSender for BudgetedSender {
    fn poll_send(
        mut self: Pin<&mut Self>,
        transmit: &noq::udp::Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        self.sender.as_mut().poll_send(transmit, cx)
    }

    fn max_transmit_segments(&self) -> NonZeroUsize {
        self.sender.max_transmit_segments()
    }
}
