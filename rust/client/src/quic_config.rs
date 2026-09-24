//! Receive-credit policy shared by HTTP/3 and WebTransport client connections.

/// Noq uses fixed per-stream receive credit. One MiB limited a continuously
/// drained download to about 80 Mbit/s on a simulated 100 ms RTT path.
const STREAM_RECEIVE_BYTES: u32 = 8 * 1024 * 1024;
const CONNECTION_RECEIVE_BYTES: u32 = 16 * 1024 * 1024;

pub(crate) fn set_receive_credit(transport: &mut quinn::TransportConfig) {
    transport.stream_receive_window(STREAM_RECEIVE_BYTES.into());
    transport.receive_window(CONNECTION_RECEIVE_BYTES.into());
}
