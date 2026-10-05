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

/// The request streams of a connection: a client's admission shares and the control streams.
pub(super) fn max_requests(limits: &Limits) -> u32 {
    let streams = limits.operations_per_client + limits.sessions_per_client + CONTROL_STREAMS;
    u32::try_from(streams).unwrap_or(u32::MAX)
}

/// The transport of every connection, whose floor noq charges when it creates one.
pub(super) fn transport(limits: &Limits) -> noq::TransportConfig {
    server_transport(max_requests(limits))
}

/// What a connection holds from accept until noq drops it: its TLS handshake and the HTTP/3 layer's state.
pub fn floor_bytes(handshake: usize) -> usize {
    handshake.saturating_add(http3::CONNECTION_BYTES)
}

/// The floor noq itself charges each connection with these limits.
pub fn noq_floor(limits: &Limits) -> usize {
    transport(limits).connection_floor_bytes()
}

/// The largest datagram the endpoint reads into one receive segment.
fn packet_bytes(config: &noq::EndpointConfig) -> Option<usize> {
    usize::try_from(config.get_max_udp_payload_size().min(64 << 10)).ok()
}

/// The endpoint's buffers: its receive batch, the first packet of each handshake awaiting accept, the queued
/// handshake packets and the socket's `kernel` buffer bytes.
pub fn endpoint_bytes(
    config: &noq::EndpointConfig,
    connections: usize,
    kernel: usize,
    segments: usize,
) -> Option<usize> {
    let packet = packet_bytes(config)?;
    noq::receive_batch_bytes(packet, segments)
        .checked_add(packet.checked_mul(connections.checked_add(1)?)?)?
        .checked_add(INCOMING_TOTAL_BYTES as usize)?
        .checked_add(kernel)
}

/// A connection's share of the budget, which noq charges: its floor until noq drops the connection, and the receive
/// credit its first funded upload reserves, which noq draws before the budget.
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

    /// Reserves the credit once, from the budget and from the share of the admitted client `keys` name; whether it is
    /// reserved.
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
