//! Error codes: HTTP/3 (RFC 9114 §8.1), QPACK (RFC 9204 §6), HTTP datagrams and WebTransport.

/// An application error code carried by RESET_STREAM, STOP_SENDING or CONNECTION_CLOSE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code(pub u64);

impl Code {
    pub const H3_NO_ERROR: Self = Self(0x100);
    pub const H3_GENERAL_PROTOCOL_ERROR: Self = Self(0x101);
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
}

const WT_FIRST: u64 = 0x52e4a40fa8db;
const WT_LAST: u64 = 0x52e5ac983162;

/// A WebTransport application error code. Firefox reads only 8 bits, so codes above 255 are unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WtCode(pub u8);

impl WtCode {
    pub fn to_http(self) -> Code {
        Code(to_http(self.0.into()))
    }

    /// `None` for codes outside the WebTransport range, reserved codepoints and codes above 255.
    pub fn from_http(code: Code) -> Option<Self> {
        from_http(code.0).and_then(|code| u8::try_from(code).ok()).map(Self)
    }
}

/// The draft's mapping skips one reserved codepoint every 0x1f.
fn to_http(code: u32) -> u64 {
    WT_FIRST + u64::from(code) + u64::from(code) / 0x1e
}

fn from_http(code: u64) -> Option<u32> {
    if !(WT_FIRST..=WT_LAST).contains(&code) || (code - 0x21).is_multiple_of(0x1f) {
        return None;
    }
    let shifted = code - WT_FIRST;
    u32::try_from(shifted - shifted / 0x1f).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webtransport_application_codes_map_like_quiche() {
        for (code, http) in [
            (0, 0x52e4a40fa8db),
            (0x1d, 0x52e4a40fa8f8),
            (0x1e, 0x52e4a40fa8fa),
            (0xff, 0x52e4a40fa9e2),
            (0xffff_ffff, 0x52e5ac983162),
        ] {
            assert_eq!(to_http(code), http);
            assert_eq!(from_http(http), Some(code));
        }
        for reserved in [0x52e4a40fa8f9, WT_FIRST - 1, WT_LAST + 1] {
            assert_eq!(from_http(reserved), None);
        }
        assert_eq!(WtCode::from_http(Code(0x52e4a40fa9e2)), Some(WtCode(0xff)));
        assert_eq!(WtCode::from_http(Code(0x52e4a40fa9e3)), None);
    }
}
