//! HTTP/3 over QUIC: endpoints holding buffers from bind, Retry under pressure, connection floors, the driver.

mod budget;
mod endpoint;
mod request;
mod shard;
mod window;

pub(crate) use budget::{endpoint_bytes, floor_bytes, noq_floor};
pub use endpoint::{Binding, Endpoints};

use super::{
    PEER_FAILURES,
    accept::Listen,
    lifecycle::{Event, Grace, Lifecycle},
    tls::Certificates,
};
use crate::{
    app::{App, Connection, Endpoint},
    limits::Lease,
};
use budget::ConnectionBudget;
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_http3::{self as http3, server};
use graphite_meter_proto::{lane::LaneEnding, text::quote};
use request::Requests;
use std::{
    future::{Future, poll_fn},
    io,
    net::SocketAddr,
    pin::pin,
    sync::Arc,
    time::Duration,
};
use tokio::{
    runtime::Handle,
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_util::sync::CancellationToken;
use window::{SendWindow, Window};

/// A whole handshake has this long.
const HANDSHAKE_BOUND: Duration = Duration::from_secs(10);
/// How often a connection's send window follows its demand.
const SEND_WINDOW_TUNING: Duration = Duration::from_millis(250);
/// Connections closed once the shutdown drain ended have this long to send their close.
const CLOSE_FLUSH: Duration = Duration::from_secs(1);

/// An endpoint's accepts; dropping it refuses new connections while running ones go on.
pub struct Listener {
    endpoint: noq::Endpoint,
    app: Arc<App>,
    runtime: Handle,
}

/// What the endpoints' connections share.
#[derive(Clone)]
pub struct Http3 {
    app: Arc<App>,
    config: noq::ServerConfig,
    certificates: Arc<Certificates>,
    max_requests: usize,
    shutdown: CancellationToken,
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

    /// The next handshake, or `None` once Retry answered it: a round trip protects admission from spoofed Initials.
    async fn accept(&mut self) -> io::Result<Option<(noq::Incoming, SocketAddr)>> {
        let incoming = self.endpoint.accept().await.ok_or(io::ErrorKind::InvalidInput)?;
        let peer = incoming.remote_address();
        if !incoming.remote_address_validated() && self.app.quic_retry(peer.ip().to_canonical()) {
            let _ = incoming.retry();
            return Ok(None);
        }
        Ok(Some((incoming, peer)))
    }

    fn name(&self) -> String {
        self.local_addr()
            .map_or_else(|_| "udp".into(), |address| format!("udp {address}"))
    }

    /// Closes the connections the drain left with H3_NO_ERROR and lets the closes reach the socket.
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

    /// Serves requests until the layer or lifecycle ends the connection; a stop closes every session first.
    async fn serve(
        &self,
        quic: noq::Connection,
        budget: Arc<ConnectionBudget>,
        peer: SocketAddr,
    ) -> Result<(), http3::Error> {
        let connection = Connection::new(Endpoint::Quic, peer.ip());
        let window = Arc::new(Window::new(self.app.clone(), quic.clone(), budget.clone()));
        let requests = Requests::new(self.app.clone(), connection.clone(), window.clone(), quic.clone());
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

/// Whether a connection ended without a failure worth logging: a peer's close or an idle timeout.
fn ended_normally(error: &http3::Error) -> bool {
    use noq::ConnectionError::{ConnectionClosed, LocallyClosed, TimedOut};
    match error {
        http3::Error::Connection { local, .. } => !local,
        http3::Error::Transport(TimedOut | LocallyClosed) => true,
        http3::Error::Transport(ConnectionClosed(close)) => close.error_code == noq::TransportErrorCode::NO_ERROR,
        _ => false,
    }
}
