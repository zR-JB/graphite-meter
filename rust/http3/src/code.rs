//! Defines the error codes of HTTP/3 (RFC 9114 §8.1), QPACK (RFC 9204 §6), HTTP datagrams and WebTransport.

/// An application error code travels in RESET_STREAM, STOP_SENDING or CONNECTION_CLOSE and is always a varint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code(pub(crate) u64);

impl Code {
    pub const H3_NO_ERROR: Self = Self(0x100);
    pub const H3_STREAM_CREATION_ERROR: Self = Self(0x103);
    pub const H3_CLOSED_CRITICAL_STREAM: Self = Self(0x104);
    pub const H3_FRAME_UNEXPECTED: Self = Self(0x105);
    pub const H3_FRAME_ERROR: Self = Self(0x106);
    pub const H3_EXCESSIVE_LOAD: Self = Self(0x107);
    pub const H3_ID_ERROR: Self = Self(0x108);
    pub const H3_SETTINGS_ERROR: Self = Self(0x109);
    pub const H3_MISSING_SETTINGS: Self = Self(0x10a);
    pub const H3_REQUEST_REJECTED: Self = Self(0x10b);
    pub const H3_REQUEST_CANCELLED: Self = Self(0x10c);
    pub const H3_REQUEST_INCOMPLETE: Self = Self(0x10d);
    pub const H3_MESSAGE_ERROR: Self = Self(0x10e);
    pub const QPACK_DECOMPRESSION_FAILED: Self = Self(0x200);
    pub const QPACK_ENCODER_STREAM_ERROR: Self = Self(0x201);
    pub const QPACK_DECODER_STREAM_ERROR: Self = Self(0x202);
    pub const H3_DATAGRAM_ERROR: Self = Self(0x33);
    pub const WT_BUFFERED_STREAM_REJECTED: Self = Self(0x3994bd84);
    pub const WT_SESSION_GONE: Self = Self(0x170d7b68);
    /// A cancelled lane resets with this, WebTransport application code 0.
    pub const WT_CANCELLED: Self = Self(0x52e4a40fa8db);
}

impl From<noq::VarInt> for Code {
    fn from(code: noq::VarInt) -> Self {
        Self(code.into_inner())
    }
}

impl From<Code> for noq::VarInt {
    fn from(code: Code) -> Self {
        Self::from_u64(code.0).expect("codes are varints")
    }
}
