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
    /// The peer's SETTINGS do not enable WebTransport, so no session starts.
    NoWebTransport,
    /// After the server's GOAWAY no request or session starts on the connection (RFC 9114 §5.2).
    GoingAway,
    /// Refused locally: over a limit or the budget, or in an ended session.
    Refused,
}

impl Error {
    /// This side closed the connection with `code`.
    pub(crate) fn local(code: Code) -> Self {
        Self::Connection { local: true, code, reason: Bytes::new() }
    }

    /// A normal close: H3_NO_ERROR, a peer's 0, which some servers stop with, or an application close below the layer.
    pub(crate) fn graceful(&self) -> bool {
        match self {
            Self::Connection { local, code, .. } => *code == Code::H3_NO_ERROR || !local && *code == Code(0),
            Self::Transport(error) => *error == noq::ConnectionError::LocallyClosed,
            _ => false,
        }
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
            Self::NoWebTransport => f.write_str("peer did not negotiate WebTransport"),
            Self::GoingAway => f.write_str("HTTP/3 connection going away after the server's GOAWAY"),
            Self::Refused => f.write_str("HTTP/3 request refused"),
        }
    }
}

/// Display already names a QUIC failure, so its cause is the failure's own.
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => error.source(),
            Self::Reset(_)
            | Self::Stopped(_)
            | Self::Protocol(_)
            | Self::Connection { .. }
            | Self::TimedOut
            | Self::NoWebTransport
            | Self::GoingAway
            | Self::Refused => None,
        }
    }
}

impl From<noq::ConnectionError> for Error {
    fn from(error: noq::ConnectionError) -> Self {
        match error {
            noq::ConnectionError::ApplicationClosed(close) => Self::Connection {
                local: false,
                code: close.error_code.into(),
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
            noq::ReadError::Reset(code) => Self::Reset(code.into()),
            noq::ReadError::ConnectionLost(error) => error.into(),
            noq::ReadError::ClosedStream | noq::ReadError::ZeroRttRejected => Self::Reset(Code::H3_REQUEST_CANCELLED),
        }
    }
}

impl From<noq::WriteError> for Error {
    fn from(error: noq::WriteError) -> Self {
        match error {
            noq::WriteError::Stopped(code) => Self::Stopped(code.into()),
            noq::WriteError::ConnectionLost(error) => error.into(),
            noq::WriteError::ClosedStream | noq::WriteError::ZeroRttRejected => {
                Self::Stopped(Code::H3_REQUEST_CANCELLED)
            }
        }
    }
}

impl From<noq::SendDatagramError> for Error {
    fn from(error: noq::SendDatagramError) -> Self {
        match error {
            noq::SendDatagramError::ConnectionLost(error) => error.into(),
            noq::SendDatagramError::UnsupportedByPeer
            | noq::SendDatagramError::Disabled
            | noq::SendDatagramError::TooLarge => Self::Refused,
        }
    }
}
