//! One error type for requests, sessions and connections, mapped once from noq's.
use crate::code::Code;
use bytes::Bytes;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The peer reset the stream this side reads.
    Reset(Code),
    /// The peer stopped the stream this side writes.
    Stopped(Code),
    /// The peer broke HTTP/3 on this stream, which the layer aborted with this code.
    Protocol(Code),
    /// The connection closed with an HTTP/3 code, by the peer or by the layer on a violation.
    Connection { local: bool, code: Code, reason: Bytes },
    /// QUIC failed below HTTP/3.
    Transport(noq::ConnectionError),
    /// A protocol deadline passed.
    TimedOut,
    /// Refused locally: over a limit or the budget, or after GOAWAY.
    Refused,
}

impl Error {
    /// Both codes end a connection gracefully: the Go server stops with 0.
    pub(crate) fn graceful(&self) -> bool {
        matches!(
            self,
            Self::Connection {
                local: false,
                code: Code(0) | Code::H3_NO_ERROR,
                ..
            }
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reset(code) => write!(f, "HTTP/3 stream reset by peer: {:#x}", code.0),
            Self::Stopped(code) => write!(f, "HTTP/3 stream stopped by peer: {:#x}", code.0),
            Self::Protocol(code) => write!(f, "HTTP/3 stream aborted: {:#x}", code.0),
            Self::Connection { local, code, reason } => write!(
                f,
                "HTTP/3 connection closed by {}: {:#x} {}",
                if *local { "this side" } else { "peer" },
                code.0,
                String::from_utf8_lossy(reason)
            ),
            Self::Transport(error) => write!(f, "QUIC connection failed: {error}"),
            Self::TimedOut => f.write_str("HTTP/3 deadline passed"),
            Self::Refused => f.write_str("HTTP/3 request refused"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Code> for noq::VarInt {
    fn from(code: Code) -> Self {
        Self::from_u64(code.0).expect("HTTP/3 codes are varints")
    }
}

impl From<noq::ConnectionError> for Error {
    fn from(error: noq::ConnectionError) -> Self {
        match error {
            noq::ConnectionError::ApplicationClosed(close) => Self::Connection {
                local: false,
                code: Code(close.error_code.into_inner()),
                reason: close.reason,
            },
            error => Self::Transport(error),
        }
    }
}

// The layer never touches a stream it closed, and accepts no 0-RTT.
impl From<noq::ReadError> for Error {
    fn from(error: noq::ReadError) -> Self {
        match error {
            noq::ReadError::Reset(code) => Self::Reset(Code(code.into_inner())),
            noq::ReadError::ConnectionLost(error) => error.into(),
            noq::ReadError::ClosedStream | noq::ReadError::ZeroRttRejected => Self::Reset(Code::H3_REQUEST_CANCELLED),
        }
    }
}

impl From<noq::WriteError> for Error {
    fn from(error: noq::WriteError) -> Self {
        match error {
            noq::WriteError::Stopped(code) => Self::Stopped(Code(code.into_inner())),
            noq::WriteError::ConnectionLost(error) => error.into(),
            noq::WriteError::ClosedStream | noq::WriteError::ZeroRttRejected => {
                Self::Stopped(Code::H3_REQUEST_CANCELLED)
            }
        }
    }
}
