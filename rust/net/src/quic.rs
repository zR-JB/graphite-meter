//! Builds the QUIC transport settings for both roles and holds the shared receive and send windows.
use graphite_meter_proto::lane::IDLE_BOUND;
use noq::{IdleTimeout, TransportConfig, VarInt};

/// Autotuning grows each stream's receive window to this size.
const STREAM_RECEIVE_WINDOW: u32 = 32 << 20;
/// Autotuning grows each connection's receive window to this size.
pub const RECEIVE_WINDOW: u32 = 48 << 20;
/// A connection's receive window starts here, and autotuning grows it from there.
pub const INITIAL_RECEIVE_WINDOW: u32 = 768 << 10;
/// A server connection's receive window stays at one maximal HTTP/3 frame until an admitted upload reads.
pub const RECEIVE_WINDOW_FLOOR: u32 = 64 << 10;
/// A server connection starts its send window here and tunes back to it.
pub const MIN_SEND_WINDOW: u64 = 2 << 20;
/// The client sends with this window, and the server's tuning stops here.
pub const MAX_SEND_WINDOW: u64 = 16 << 20;

/// A client accepts this many unidirectional streams from a server.
const CLIENT_UNI_STREAMS: u32 = 36;
/// A server grants the client's three HTTP/3 streams, the 16-lane WebTransport cap and four streams of credit headroom.
const SERVER_UNI_STREAMS: u32 = 3 + 16 + 4;
const CLIENT_DATAGRAM_BUFFER: usize = 256 << 10;
const SERVER_DATAGRAM_BUFFER: usize = 64 << 10;
const CLIENT_IDLE_MILLIS: u32 = 60_000;

/// Configures a client connection's transport, which lets the server open only unidirectional streams.
pub fn client_transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(0_u32.into());
    transport.max_concurrent_uni_streams(CLIENT_UNI_STREAMS.into());
    transport.stream_receive_window(STREAM_RECEIVE_WINDOW.into());
    transport.receive_window(RECEIVE_WINDOW.into());
    transport.initial_receive_window(Some(INITIAL_RECEIVE_WINDOW.into()));
    transport.send_window(MAX_SEND_WINDOW);
    transport.datagram_receive_buffer_size(Some(CLIENT_DATAGRAM_BUFFER));
    transport.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(CLIENT_IDLE_MILLIS))));
    transport
}

/// Configures a server connection's transport to accept `requests` request streams and idle out after the lane bound.
pub fn server_transport(requests: u32) -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(requests.into());
    transport.max_concurrent_uni_streams(SERVER_UNI_STREAMS.into());
    transport.stream_receive_window(STREAM_RECEIVE_WINDOW.into());
    transport.receive_window(RECEIVE_WINDOW_FLOOR.into());
    transport.initial_receive_window(Some(INITIAL_RECEIVE_WINDOW.into()));
    transport.send_window(MIN_SEND_WINDOW);
    transport.datagram_receive_buffer_size(Some(SERVER_DATAGRAM_BUFFER));
    transport.datagram_send_buffer_size(SERVER_DATAGRAM_BUFFER);
    const IDLE_MILLIS: u32 = {
        let millis = IDLE_BOUND.as_millis();
        assert!(millis <= u32::MAX as u128, "the lane bound fits QUIC's idle timeout");
        millis as u32
    };
    transport.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(IDLE_MILLIS))));
    transport
}
