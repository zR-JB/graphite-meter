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
use graphite_meter_net::quic::{
    MAX_SEND_WINDOW, MIN_SEND_WINDOW, RECEIVE_WINDOW, RECEIVE_WINDOW_FLOOR, server_transport,
};
use std::{
    sync::{Arc, Mutex},
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
    let streams = limits
        .operations_per_client
        .checked_add(limits.sessions_per_client.get());
    let streams =
        streams.and_then(|streams| u32::try_from(streams.get().checked_add(CONTROL_STREAMS)?).ok());
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
    let queue = if shards > 1 {
        super::shard::forwarding_bytes(packet)?
    } else {
        0
    };
    noq::receive_batch_bytes(packet, segments)
        .checked_add(packet.checked_mul(connections.div_ceil(shards).checked_add(1)?)?)?
        .checked_add((INCOMING_TOTAL_BYTES as usize).div_ceil(shards))?
        .checked_add(queue)?
        .checked_add(kernel)
}

/// A connection's noq-charged share: its floor until dropped, and a funded upload's credit while uploads read.
#[derive(Debug)]
pub(super) struct ConnectionBudget {
    budget: Budget,
    shared: Arc<dyn noq::SharedBudget>,
    _floor: Lease,
    held: Mutex<Held>,
}

#[derive(Debug, Default)]
struct Held {
    credit: Option<(Lease, Hold)>,
    undrawn: usize,
    overdraft: usize,
    /// Admitted uploads reading on the connection, funded or not.
    readers: usize,
}

impl ConnectionBudget {
    pub(super) fn new(budget: &Budget, floor: Lease) -> Self {
        Self {
            budget: budget.clone(),
            shared: budget.noq(),
            _floor: floor,
            held: Mutex::default(),
        }
    }

    /// Reserves the credit, from the budget and the share of the client `keys` name; whether it is reserved.
    pub(super) fn reserve(&self, app: &App, keys: &ClientKeys) -> bool {
        let mut held = lock(&self.held);
        if held.credit.is_none()
            && let Some(share) = app.window_credit(keys, CONNECTION_CREDIT)
            && let Some(credit) = self.budget.lease(CONNECTION_CREDIT)
        {
            held.undrawn += CONNECTION_CREDIT;
            held.credit = Some((credit, share));
        }
        held.credit.is_some()
    }

    pub(super) fn reserved(&self) -> bool {
        lock(&self.held).credit.is_some()
    }

    /// An admitted upload began or stopped reading; whether the last to stop returned the credit, the bytes it still
    /// covers moving to the budget. Under pressure the connection keeps it.
    fn reading(&self, reading: bool) -> bool {
        let mut held = lock(&self.held);
        if reading {
            held.readers += 1;
            return false;
        }
        held.readers -= 1;
        let drawn = CONNECTION_CREDIT - held.undrawn;
        if held.readers > 0 || held.credit.is_none() || !self.shared.try_charge(drawn) {
            return false;
        }
        held.overdraft += drawn;
        (held.undrawn, held.credit) = (0, None);
        true
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

const SEND_WINDOW_STEP: u64 = 256 << 10;
const SEND_WINDOW_SHRINK_DELAY: Duration = Duration::from_secs(1);

/// The receive window: 64 KiB, then while funded uploads read autotuned 768 KiB to 48 MiB on their client's credit.
pub(super) struct QuicWindow {
    app: Arc<App>,
    quic: noq::Connection,
    budget: Arc<ConnectionBudget>,
}

impl QuicWindow {
    pub(super) fn new(app: Arc<App>, quic: noq::Connection, budget: Arc<ConnectionBudget>) -> Self {
        Self { app, quic, budget }
    }

    pub(super) fn fund(&self, keys: &ClientKeys) -> bool {
        let reserved = self.budget.reserve(&self.app, keys);
        if reserved {
            self.quic.set_receive_window(RECEIVE_WINDOW.into());
        }
        reserved
    }

    /// An admitted upload reads on the connection until the guard drops.
    pub(super) fn reader(&self) -> Reader<'_> {
        self.read(true);
        Reader(self)
    }

    /// Lowers the window once the credit is returned, raising it again if an upload funded it meanwhile.
    fn read(&self, reading: bool) {
        if self.budget.reading(reading) {
            self.quic.set_receive_window(RECEIVE_WINDOW_FLOOR.into());
            if self.budget.reserved() {
                self.quic.set_receive_window(RECEIVE_WINDOW.into());
            }
        }
    }
}

pub(super) struct Reader<'a>(&'a QuicWindow);

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        self.0.read(false);
    }
}

impl ReceiveWindow for QuicWindow {
    type Stream = RecvHalf;
    type Error = http3::Error;

    fn budget(&self) -> &Budget {
        self.app.budget()
    }

    fn raise(&self, _: &mut RecvHalf, keys: &ClientKeys) -> bool {
        self.fund(keys)
    }

    fn reading(&self, _: &mut RecvHalf, reading: bool) {
        self.read(reading);
    }

    fn poll_data(
        stream: &mut RecvHalf,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, http3::Error>>> {
        stream.poll_data(cx).map(Result::transpose)
    }
}

/// The send window: 2 MiB, toward two BDPs up to 16 MiB with headroom, back to 2 MiB after 1 s of low demand.
pub(super) struct SendWindow {
    limit: u64,
    last: Option<(Instant, u64)>,
    low_since: Option<Instant>,
}

impl Default for SendWindow {
    fn default() -> Self {
        Self {
            limit: MIN_SEND_WINDOW,
            last: None,
            low_since: None,
        }
    }
}

impl SendWindow {
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
            let target = desired(
                sent.saturating_sub(previous),
                path.rtt,
                now.duration_since(last),
            );
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
        if self.widen(target, budget) {
            quic.set_send_window(self.limit);
        }
    }

    fn widen(&mut self, target: u64, budget: &Budget) -> bool {
        let granted = target.min(MAX_SEND_WINDOW).saturating_sub(self.limit);
        let widened = granted >= SEND_WINDOW_STEP && budget.pressure() != Pressure::HoldBack;
        if widened {
            self.limit += granted;
        }
        widened
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
    let sent_rtt = u128::from(sent).saturating_mul(rtt.as_nanos());
    let Some(demand) = sent_rtt.saturating_mul(2).checked_div(elapsed.as_nanos()) else {
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
        assert_eq!(
            desired(1 << 20, millis(1), Duration::ZERO),
            MIN_SEND_WINDOW,
            "no time observed"
        );
    }

    #[test]
    fn the_send_window_grows_only_below_three_quarters_of_the_budget() {
        let budget = Budget::new(64 << 20);
        let pressure = budget.lease(48 << 20).unwrap();
        let mut window = SendWindow::default();
        assert!(!window.widen(MAX_SEND_WINDOW, &budget));
        assert_eq!(window.limit, MIN_SEND_WINDOW);
        drop(pressure);
        assert!(window.widen(MAX_SEND_WINDOW, &budget));
        assert_eq!(window.limit, MAX_SEND_WINDOW);
    }
}
