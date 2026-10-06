//! What QUIC holds of the buffer budget, and a connection's receive and send windows.

use crate::{
    app::App,
    config::Limits,
    limits::{Budget, CONNECTION_CREDIT, Hold, Lease, Pressure},
    lock,
    peer::ClientKeys,
    transport::window::ReceiveWindow,
};
use bytes::Bytes;
use graphite_meter_http3::{self as http3, RecvHalf};
use graphite_meter_net::quic::{MAX_SEND_WINDOW, MIN_SEND_WINDOW, RECEIVE_WINDOW, server_transport};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::Instant;

/// Request streams past a client's admission shares, for its control requests.
const CONTROL_STREAMS: usize = 4;
/// Handshake packets the endpoint queues for one incoming connection, and for all of them.
pub(super) const INCOMING_BYTES: u64 = 64 << 10;
pub(super) const INCOMING_TOTAL_BYTES: u64 = 4 << 20;

/// A connection's request streams: a client's admission shares and control streams, if a QUIC stream count fits.
pub(super) fn max_requests(limits: &Limits) -> Result<u32, String> {
    let streams = limits.operations_per_client.checked_add(limits.sessions_per_client);
    let streams = streams.and_then(|streams| u32::try_from(streams.checked_add(CONTROL_STREAMS)?).ok());
    streams.ok_or_else(|| "per-client stream budgets exceed the QUIC stream limit".into())
}

/// The transport of every connection, whose floor noq charges when it creates one.
pub(super) fn transport(limits: &Limits) -> Result<noq::TransportConfig, String> {
    max_requests(limits).map(server_transport)
}

/// What a connection holds from accept until noq drops it: its TLS handshake and the HTTP/3 layer's state.
pub fn floor_bytes(handshake: usize) -> usize {
    handshake.saturating_add(http3::CONNECTION_BYTES)
}

/// The floor noq itself charges each connection with these limits.
pub fn noq_floor(limits: &Limits) -> Result<usize, String> {
    transport(limits).map(|transport| transport.connection_floor_bytes())
}

/// The largest datagram an endpoint reads into one receive segment.
pub(super) fn packet_bytes(config: &noq::EndpointConfig) -> Option<usize> {
    usize::try_from(config.get_max_udp_payload_size().min(64 << 10)).ok()
}

/// One of `shards` endpoints' buffers: receive batch, handshake and queued packets, forwarding, `kernel` bytes.
pub fn endpoint_bytes(
    config: &noq::EndpointConfig,
    shards: usize,
    connections: usize,
    kernel: usize,
    segments: usize,
) -> Option<usize> {
    let packet = packet_bytes(config)?;
    let queue = if shards > 1 { super::shard::queue_bytes(packet)? } else { 0 };
    noq::receive_batch_bytes(packet, segments)
        .checked_add(packet.checked_mul(connections.div_ceil(shards).checked_add(1)?)?)?
        .checked_add((INCOMING_TOTAL_BYTES as usize).div_ceil(shards))?
        .checked_add(queue)?
        .checked_add(kernel)
}

/// A connection's noq-charged share: its floor until dropped, and its first funded upload's credit.
#[derive(Debug)]
pub(super) struct ConnectionBudget {
    budget: Budget,
    shared: Arc<dyn noq::SharedBudget>,
    _floor: Lease,
    held: Mutex<Held>,
    reserved: AtomicBool,
}

#[derive(Debug, Default)]
struct Held {
    /// The credit and the share of the client whose upload reserved it.
    credit: Option<(Lease, Hold)>,
    undrawn: usize,
    overdraft: usize,
}

impl ConnectionBudget {
    pub(super) fn new(budget: &Budget, floor: Lease) -> Self {
        Self {
            budget: budget.clone(),
            shared: budget.noq(),
            _floor: floor,
            held: Mutex::default(),
            reserved: AtomicBool::new(false),
        }
    }

    /// Reserves the credit once, from the budget and the share of the client `keys` name; whether it is reserved.
    pub(super) fn reserve(&self, app: &App, keys: &ClientKeys) -> bool {
        let mut held = lock(&self.held);
        if held.credit.is_none()
            && let Some(share) = app.window_credit(keys, CONNECTION_CREDIT)
            && let Some(credit) = self.budget.lease(CONNECTION_CREDIT)
        {
            held.undrawn += CONNECTION_CREDIT;
            held.credit = Some((credit, share));
            self.reserved.store(true, Ordering::Relaxed);
        }
        held.credit.is_some()
    }

    /// Whether the credit is reserved, which it stays until the connection is gone.
    pub(super) fn reserved(&self) -> bool {
        self.reserved.load(Ordering::Relaxed)
    }
}

impl noq::SharedBudget for ConnectionBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        let mut held = lock(&self.held);
        let credit = held.undrawn.min(bytes);
        if credit < bytes && !self.shared.try_charge(bytes - credit) {
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
        self.shared.refund(overdraft);
    }
}

/// The least growth of a send window worth a change.
const SEND_WINDOW_STEP: u64 = 256 << 10;
/// A send window shrinks after its demand stayed low this long.
const SEND_WINDOW_SHRINK_DELAY: Duration = Duration::from_secs(1);

/// The receive window: 64 KiB, then once an upload reads autotuned 768 KiB to 48 MiB on its client's credit.
pub(super) struct Window {
    app: Arc<App>,
    quic: noq::Connection,
    budget: Arc<ConnectionBudget>,
}

impl Window {
    pub(super) fn new(app: Arc<App>, quic: noq::Connection, budget: Arc<ConnectionBudget>) -> Self {
        Self { app, quic, budget }
    }

    /// Whether an upload of `keys` reads at the raised window, raising it for the first.
    pub(super) fn fund(&self, keys: &ClientKeys) -> bool {
        let reserved = self.budget.reserve(&self.app, keys);
        if reserved {
            self.quic.set_receive_window(RECEIVE_WINDOW.into());
        }
        reserved
    }

    /// Whether the window was raised.
    pub(super) fn raised(&self) -> bool {
        self.budget.reserved()
    }
}

impl ReceiveWindow for Window {
    type Stream = RecvHalf;
    type Error = http3::Error;

    fn budget(&self) -> &Budget {
        self.app.budget()
    }

    fn raise(&self, _: &mut RecvHalf, keys: &ClientKeys) -> bool {
        self.fund(keys)
    }

    fn poll_data(stream: &mut RecvHalf, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, http3::Error>>> {
        stream.poll_data(cx).map(Result::transpose)
    }
}

/// The send window: 2 MiB, toward two BDPs up to 16 MiB with headroom, back to 2 MiB after 1 s of low demand.
pub(super) struct SendWindow {
    limit: u64,
    /// When the path's sent bytes were last read, and their count.
    last: Option<(Instant, u64)>,
    low_since: Option<Instant>,
}

impl Default for SendWindow {
    fn default() -> Self {
        Self { limit: MIN_SEND_WINDOW, last: None, low_since: None }
    }
}

impl SendWindow {
    /// Follows the connection's demand while requests run.
    pub(super) fn tune(&mut self, quic: &noq::Connection, busy: bool, budget: &Budget) {
        if !busy {
            return self.release(quic);
        }
        // The total first, so a send on the first path in between cannot look like another path's traffic.
        let all_sent = quic.stats().udp_tx.bytes;
        let path = quic.path_stats(noq::PathId::ZERO);
        let Some(path) = path.filter(|path| all_sent <= path.udp_tx.bytes) else {
            (self.last, self.low_since) = (None, None);
            return;
        };
        let (now, sent) = (Instant::now(), path.udp_tx.bytes);
        if let Some((last, previous)) = self.last {
            let target = desired(sent.saturating_sub(previous), path.rtt, now.duration_since(last));
            if target == MIN_SEND_WINDOW && self.limit != MIN_SEND_WINDOW {
                let since = *self.low_since.get_or_insert(now);
                if now.duration_since(since) >= SEND_WINDOW_SHRINK_DELAY {
                    self.release(quic);
                }
            } else {
                self.low_since = None;
                self.grow(quic, target, budget);
            }
        }
        self.last = Some((now, sent));
    }

    fn grow(&mut self, quic: &noq::Connection, target: u64, budget: &Budget) {
        let granted = target.min(MAX_SEND_WINDOW).saturating_sub(self.limit);
        if granted >= SEND_WINDOW_STEP && budget.pressure() != Pressure::HoldBack {
            self.limit += granted;
            quic.set_send_window(self.limit);
        }
    }

    fn release(&mut self, quic: &noq::Connection) {
        (self.last, self.low_since) = (None, None);
        if self.limit != MIN_SEND_WINDOW {
            self.limit = MIN_SEND_WINDOW;
            quic.set_send_window(MIN_SEND_WINDOW);
        }
    }
}

/// Two bandwidth-delay products of `sent` over `elapsed`, within the send window's bounds, sparing the queue.
fn desired(sent: u64, rtt: Duration, elapsed: Duration) -> u64 {
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
    use super::*;

    #[test]
    fn the_send_window_wants_two_bandwidth_delay_products_within_its_bounds() {
        let millis = Duration::from_millis;
        assert_eq!(desired(16 << 20, millis(50), millis(250)), 6_710_886);
        assert_eq!(desired(16 << 20, millis(100), millis(100)), MAX_SEND_WINDOW);
        assert_eq!(desired(1 << 20, millis(1), millis(250)), MIN_SEND_WINDOW);
        assert_eq!(desired(1 << 20, millis(1), Duration::ZERO), MIN_SEND_WINDOW, "no time observed");
    }
}
