//! What QUIC holds of the buffer budget: each connection's floor and receive credit, and the endpoint's buffers.

use crate::{
    app::App,
    config::Limits,
    limits::{Budget, CONNECTION_CREDIT, Hold, Lease},
    lock,
    peer::ClientKeys,
};
use graphite_meter_http3 as http3;
use graphite_meter_net::quic::server_transport;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

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
