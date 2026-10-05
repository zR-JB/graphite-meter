//! What QUIC holds of the buffer budget: each connection's floor and the endpoint's buffers.

use crate::config::Limits;
use graphite_meter_http3 as http3;
use graphite_meter_net::quic::server_transport;

/// Request streams past a client's admission shares, for its control requests.
const CONTROL_STREAMS: usize = 4;
/// Handshake packets the endpoint queues for all incoming connections.
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
