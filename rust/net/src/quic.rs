//! QUIC transport settings for both roles, with the shared receive and send windows.
use graphite_meter_proto::lane::IDLE_BOUND;
use noq::{IdleTimeout, TransportConfig, VarInt};

/// The receive window autotuning grows each stream to.
pub const STREAM_RECEIVE_WINDOW: u32 = 32 << 20;
/// The receive window autotuning grows each connection to.
pub const RECEIVE_WINDOW: u32 = 48 << 20;
/// quic-go's first connection receive window, from which autotuning starts.
pub const INITIAL_RECEIVE_WINDOW: u32 = 768 << 10;
/// A server connection's receive window until an admitted upload reads: one maximal HTTP/3 frame.
pub const RECEIVE_WINDOW_FLOOR: u32 = 64 << 10;
/// The send window a server connection starts at and tunes back to.
pub const MIN_SEND_WINDOW: u64 = 2 << 20;
/// The client's send window and the ceiling of the server's tuning.
pub const MAX_SEND_WINDOW: u64 = 16 << 20;

/// The unidirectional streams a client accepts from a server.
const CLIENT_UNI_STREAMS: u32 = 36;
/// Go's three HTTP/3 streams, the 16-lane WebTransport cap and four streams of credit headroom.
const SERVER_UNI_STREAMS: u32 = 3 + 16 + 4;
const CLIENT_DATAGRAM_BUFFER: usize = 256 << 10;
const SERVER_DATAGRAM_BUFFER: usize = 64 << 10;
const CLIENT_IDLE_MILLIS: u32 = 60_000;

/// A client connection's transport: no streams opened by the server but unidirectional ones.
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

/// A server connection's transport, accepting `requests` request streams and idling out after the lane bound.
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
    let idle = u32::try_from(IDLE_BOUND.as_millis()).expect("the lane bound fits QUIC's idle timeout");
    transport.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(idle))));
    transport
}
