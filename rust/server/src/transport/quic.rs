//! HTTP/3 over QUIC: endpoints holding buffers from bind, Retry under pressure, connection floors, the driver.

mod budget;
mod endpoint;
mod request;
mod shard;

pub(crate) use budget::{endpoint_bytes, floor_bytes, noq_floor};
pub(crate) use endpoint::OVERFLOW;
pub use endpoint::{Binding, Endpoints};

use super::{PEER_FAILURES, accept::Listen, tls::Certificates};
use crate::{
    app::{App, Connection, Endpoint},
    limits::{Hold, Lease},
};
use budget::{ConnectionBudget, QuicWindow, SendWindow};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_http3::{self as http3, server};
use graphite_meter_proto::lane::{IDLE_BOUND, LaneEnding};
use request::Requests;
use shard::{Occupant, Shard};
use std::{future::Future, io, net::SocketAddr, pin::pin, sync::Arc, time::Duration};
use tokio::{
    runtime::Handle,
    time::{Instant, MissedTickBehavior, interval, timeout},
};

const HANDSHAKE_BOUND: Duration = Duration::from_secs(10);
const SEND_WINDOW_TUNING: Duration = Duration::from_millis(250);
/// Connections closed once the shutdown drain ended have this long to send their close.
const CLOSE_FLUSH: Duration = Duration::from_secs(1);
/// A connection that did no work this long was kept open by a peer sending unasked: browsers PING only for requests.
const UNASKED: Duration = IDLE_BOUND.saturating_mul(2);

/// An endpoint's accepts; dropping it refuses new connections while running ones go on.
pub struct Listener {
    endpoint: noq::Endpoint,
    app: Arc<App>,
    runtime: Handle,
    shard: Option<Shard>,
}

pub struct Arrival {
    incoming: noq::Incoming,
    occupant: Option<Occupant>,
    handshake: Hold,
}

#[derive(Clone)]
pub struct Http3 {
    app: Arc<App>,
    config: noq::ServerConfig,
    certificates: Arc<Certificates>,
    max_requests: usize,
}

impl Listener {
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    pub fn runtime(&self) -> Handle {
        self.runtime.clone()
    }
}

impl Listen for Listener {
    type Connection = Arrival;

    /// The next handshake, or `None` once Retry answered it: a round trip protects admission from spoofed Initials.
    async fn accept(&mut self) -> io::Result<Option<(Arrival, SocketAddr)>> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or(io::ErrorKind::InvalidInput)?;
        let peer = incoming.remote_address();
        let validated = incoming.remote_address_validated();
        let Some(handshake) = self.app.quic_handshake(peer.ip().to_canonical(), validated) else {
            let _ = incoming.retry();
            return Ok(None);
        };
        let occupant = self.shard.as_ref().map(Shard::occupy);
        let arrival = Arrival {
            incoming,
            occupant,
            handshake,
        };
        Ok(Some((arrival, peer)))
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
    pub fn connection(
        &self,
        arrival: Arrival,
        peer: SocketAddr,
    ) -> impl Future<Output = ()> + Send + use<> {
        let floor = floor_bytes(self.certificates.handshake_bytes());
        let budget = self
            .app
            .budget()
            .lease(floor)
            .map(|floor| self.budget(floor));
        let listener = self.clone();
        let Arrival {
            incoming,
            occupant,
            handshake,
        } = arrival;
        async move {
            let _occupant = occupant;
            let Some((budget, config)) = budget else {
                return incoming.refuse();
            };
            let Ok(connecting) = incoming.accept_with(config) else {
                return;
            };
            let address = peer.ip();
            let quic = tokio::select! {
                biased;
                () = listener.app.shutdown().cancelled() => return,
                connected = timeout(HANDSHAKE_BOUND, connecting) => match connected {
                    Ok(Ok(quic)) => quic,
                    Ok(Err(error)) => {
                        if !ended_normally(&error.clone().into()) {
                            PEER_FAILURES.write(format_args!("QUIC handshake from {address} failed: {error}"));
                        }
                        return;
                    }
                    Err(_) => {
                        PEER_FAILURES.write(format_args!("QUIC handshake from {address} failed: timed out"));
                        return;
                    }
                }
            };
            drop(handshake);
            if let Err(error) = listener.serve(quic, budget, peer).await
                && !ended_normally(&error)
            {
                PEER_FAILURES.write(format_args!(
                    "HTTP/3 connection from {address} failed: {error}"
                ));
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

    /// Serves requests until noq's idle timeout, the peer's close, a stop's drain, or `UNASKED` without work.
    async fn serve(
        &self,
        quic: noq::Connection,
        budget: Arc<ConnectionBudget>,
        peer: SocketAddr,
    ) -> Result<(), http3::Error> {
        let connection = Connection::new(Endpoint::Quic, peer.ip());
        let window = Arc::new(QuicWindow::new(
            self.app.clone(),
            quic.clone(),
            budget.clone(),
        ));
        let requests = Requests::new(
            self.app.clone(),
            connection.clone(),
            window.clone(),
            quic.clone(),
        );
        let mut http = server::Connection::new(quic.clone(), Some(budget));
        let (mut running, mut send_window) = (FuturesUnordered::new(), SendWindow::default());
        let mut tuning = interval(SEND_WINDOW_TUNING);
        tuning.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // A child token: polling it never takes the server-wide token's waiter lock.
        let shutdown = self.app.shutdown().child_token();
        let mut stopping = pin!(shutdown.cancelled());
        let mut retiring = pin!(connection.retire.cancelled());
        let (mut stopped, mut retired) = (false, false);
        let mut worked = Worked {
            at: Instant::now(),
            unacked: 0,
        };
        loop {
            tokio::select! {
                request = http.next() => match request? {
                    Some(request) if running.len() < self.max_requests => running.push(requests.serve(request)),
                    Some(request) => request.reject(),
                    None => return Ok(()),
                },
                Some(()) = running.next() => worked.at = Instant::now(),
                () = &mut stopping, if !stopped => {
                    stopped = true;
                    http.shutdown(LaneEnding::Shutdown.webtransport_code(), LaneEnding::Shutdown.reason());
                }
                () = &mut retiring, if !retired => {
                    retired = true;
                    http.retire();
                }
                _ = tuning.tick() => {
                    send_window.tune(&quic, !running.is_empty(), self.app.budget());
                    if worked.lapsed(Instant::now(), !running.is_empty(), quic.send_buffered_bytes()) {
                        quic.close(http3::Code::H3_NO_ERROR.into(), b"");
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// When a connection last worked: ran a request, or saw its unacknowledged reply bytes change.
struct Worked {
    at: Instant,
    unacked: u64,
}

impl Worked {
    fn lapsed(&mut self, now: Instant, running: bool, unacked: u64) -> bool {
        if running || unacked != self.unacked {
            (self.at, self.unacked) = (now, unacked);
        }
        now.duration_since(self.at) >= UNASKED
    }
}

/// Whether a connection ended without a failure worth logging: gracefully, by the peer's close, or idle.
fn ended_normally(error: &http3::Error) -> bool {
    error.graceful()
        || matches!(
            error,
            http3::Error::Connection { local: false, .. }
                | http3::Error::Transport(noq::ConnectionError::TimedOut)
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_works_while_a_request_runs_or_its_reply_tail_moves() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut worked = Worked {
            at: start,
            unacked: 0,
        };
        assert!(!worked.lapsed(at(30), true, 4 << 20), "a request runs");
        for (secs, unacked) in [(40, 2 << 20), (80, 1 << 20), (130, 1)] {
            assert!(
                !worked.lapsed(at(secs), false, unacked),
                "a tail moving at {secs} s"
            );
        }
        assert!(!worked.lapsed(at(189), false, 1));
        assert!(
            worked.lapsed(at(190), false, 1),
            "a tail stalled for a minute"
        );
        let mut quiet = Worked {
            at: start,
            unacked: 0,
        };
        assert!(!quiet.lapsed(at(59), false, 0));
        assert!(quiet.lapsed(at(60), false, 0), "nothing ran nor waited");
    }
}
