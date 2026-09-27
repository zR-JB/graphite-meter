//! Flow-control policy shared by HTTP/3 and WebTransport client connections.

/// Go's client ceilings; noq's credit is fixed, and a smaller window caps a 100 ms path below 1 Gbit/s.
const STREAM_RECEIVE_BYTES: u32 = 32 * 1024 * 1024;
const CONNECTION_RECEIVE_BYTES: u32 = 48 * 1024 * 1024;

pub(crate) fn set_flow_control(transport: &mut quinn::TransportConfig) {
    transport.stream_receive_window(STREAM_RECEIVE_BYTES.into());
    transport.receive_window(CONNECTION_RECEIVE_BYTES.into());
    transport.send_window(CONNECTION_RECEIVE_BYTES.into());
}
