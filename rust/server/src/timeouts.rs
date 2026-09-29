//! The server's protocol time bounds, as Go's `controlTimeout` and `wire.IdleBound` (api/wire.md#lane-endings).
use std::time::Duration;

/// Every lane's inactivity bound. It is also the idle writer's: a write the peer holds this long ends its reply.
pub use graphite_meter_core::wire::IDLE_BOUND;

/// Go's controlTimeout: an idle connection, a handshake, and every exchange until admission hands it an operation's
/// own deadlines.
pub const CONTROL: Duration = Duration::from_secs(15);
/// Connection tasks drain within this at shutdown, as does admitted work that raced a GOAWAY.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// quic-go's bound on a whole handshake: twice Go's `HandshakeIdleTimeout` of five seconds.
pub const QUIC_HANDSHAKE: Duration = Duration::from_secs(10);
/// An HTTP/2 connection's preface and first SETTINGS.
pub const H2_HANDSHAKE: Duration = Duration::from_secs(10);
/// Writing the answer to a WebTransport CONNECT that opens no session.
pub const WT_ANSWER: Duration = Duration::from_secs(10);
/// An establish-only `/wt/download?bytes=0` session closes after this linger, as Go's `wtVerifyLinger`.
pub const WT_VERIFY_LINGER: Duration = Duration::from_secs(5);
/// A `/wt/upload` refused at connect closes this long after its `error` record.
pub const WT_REFUSAL_LINGER: Duration = Duration::from_secs(2);
/// An upload progress stream sends a heartbeat line after this long without a record.
pub const PROGRESS_HEARTBEAT: Duration = Duration::from_secs(1);
/// A WebSocket close handshake, so an unresponsive peer cannot keep holding capacity.
pub const WS_CLOSE: Duration = Duration::from_secs(5);
