//! Incremental HTTP/3 WebTransport capsules. Unknown payloads are discarded in place.

use std::fmt;

const CLOSE: u64 = 0x2843;
const MAX_DATA: u64 = 0x190b4d3d;
const MAX_STREAM_DATA: u64 = 0x190b4d3e;
const MAX_BIDI: u64 = 0x190b4d3f;
const MAX_UNI: u64 = 0x190b4d40;
const DATA_BLOCKED: u64 = 0x190b4d41;
const STREAM_DATA_BLOCKED: u64 = 0x190b4d42;
const BLOCKED_BIDI: u64 = 0x190b4d43;
const BLOCKED_UNI: u64 = 0x190b4d44;
pub const MAX_STREAMS: u64 = 1 << 60;
pub const MAX_VARINT: u64 = (1 << 62) - 1;
pub const MAX_CLOSE_MESSAGE: usize = 1024;
const MAX_CLOSE_PAYLOAD: usize = 4 + MAX_CLOSE_MESSAGE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capsule {
    CloseSession { code: u32, message: Vec<u8> },
    MaxData(u64),
    MaxStreamsBidi(u64),
    MaxStreamsUni(u64),
    DataBlocked(u64),
    StreamsBlockedBidi(u64),
    StreamsBlockedUni(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    InvalidPayload,
    Http2Only,
    ValueOutOfRange,
    InvalidUtf8,
    Failed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated WebTransport capsule",
            Self::InvalidPayload => "invalid WebTransport capsule payload",
            Self::Http2Only => "HTTP/2-only WebTransport capsule received",
            Self::ValueOutOfRange => "WebTransport capsule value out of range",
            Self::InvalidUtf8 => "outgoing close message is not UTF-8",
            Self::Failed => "WebTransport capsule decoder already failed",
        })
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Default)]
enum State {
    #[default]
    Type,
    Length(u64),
    Body {
        kind: u64,
        remaining: u64,
    },
    Failed,
}

/// Holds at most 1028 payload bytes or eight header bytes, regardless of declared length.
#[derive(Debug)]
pub struct Decoder {
    state: State,
    bytes: [u8; MAX_CLOSE_PAYLOAD],
    used: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            state: State::Type,
            bytes: [0; MAX_CLOSE_PAYLOAD],
            used: 0,
        }
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Errors are terminal: subsequent calls fail until the decoder is replaced.
    pub fn feed(&mut self, input: &[u8]) -> Result<Vec<Capsule>, Error> {
        let result = self.feed_inner(input);
        if result.is_err() {
            self.state = State::Failed;
        }
        result
    }

    fn feed_inner(&mut self, mut input: &[u8]) -> Result<Vec<Capsule>, Error> {
        let mut output = Vec::new();
        loop {
            match self.state {
                State::Failed => return Err(Error::Failed),
                State::Type | State::Length(_) => {
                    if input.is_empty() {
                        break;
                    }
                    // Header storage is reused for the body once both varints are read.
                    if self.used == 0 {
                        self.bytes[0] = input[0];
                        self.used = 1;
                        input = &input[1..];
                    }
                    let size = 1 << (self.bytes[0] >> 6);
                    let take = (size - self.used).min(input.len());
                    self.bytes[self.used..self.used + take].copy_from_slice(&input[..take]);
                    self.used += take;
                    input = &input[take..];
                    if self.used != size {
                        break;
                    }
                    let value = read_varint(&self.bytes[..size]);
                    self.used = 0;
                    self.state = match self.state {
                        State::Type => State::Length(value),
                        State::Length(kind) => {
                            if matches!(kind, MAX_STREAM_DATA | STREAM_DATA_BLOCKED) {
                                return Err(Error::Http2Only);
                            }
                            if (kind == CLOSE && value < 4)
                                || (is_numeric(kind) && !(1..=8).contains(&value))
                            {
                                return Err(Error::InvalidPayload);
                            }
                            State::Body {
                                kind,
                                remaining: value,
                            }
                        }
                        _ => unreachable!(),
                    };
                }
                State::Body { kind, remaining } => {
                    let take = usize::try_from(remaining)
                        .unwrap_or(usize::MAX)
                        .min(input.len());
                    // Unknown bodies and oversized close tails are consumed without storage.
                    let retained_limit = if kind == CLOSE {
                        MAX_CLOSE_PAYLOAD
                    } else if is_numeric(kind) {
                        8
                    } else {
                        0
                    };
                    let retained_bytes = take.min(retained_limit - self.used);
                    self.bytes[self.used..self.used + retained_bytes]
                        .copy_from_slice(&input[..retained_bytes]);
                    self.used += retained_bytes;
                    input = &input[take..];
                    let remaining = remaining - take as u64;
                    if remaining != 0 {
                        self.state = State::Body { kind, remaining };
                        break;
                    }
                    if kind == CLOSE {
                        output.push(Capsule::CloseSession {
                            code: u32::from_be_bytes(self.bytes[..4].try_into().unwrap()),
                            message: self.bytes[4..self.used].to_vec(),
                        });
                    } else if is_numeric(kind) {
                        if self.used != 1 << (self.bytes[0] >> 6) {
                            return Err(Error::InvalidPayload);
                        }
                        let value = read_varint(&self.bytes[..self.used]);
                        if is_stream_count(kind) && value > MAX_STREAMS {
                            return Err(Error::ValueOutOfRange);
                        }
                        output.push(match kind {
                            MAX_DATA => Capsule::MaxData(value),
                            MAX_BIDI => Capsule::MaxStreamsBidi(value),
                            MAX_UNI => Capsule::MaxStreamsUni(value),
                            DATA_BLOCKED => Capsule::DataBlocked(value),
                            BLOCKED_BIDI => Capsule::StreamsBlockedBidi(value),
                            BLOCKED_UNI => Capsule::StreamsBlockedUni(value),
                            _ => unreachable!(),
                        });
                    }
                    self.used = 0;
                    self.state = State::Type;
                }
            }
        }
        Ok(output)
    }

    pub fn finish(&self) -> Result<(), Error> {
        match self.state {
            State::Type if self.used == 0 => Ok(()),
            State::Failed => Err(Error::Failed),
            _ => Err(Error::Truncated),
        }
    }
}

fn is_numeric(kind: u64) -> bool {
    matches!(
        kind,
        MAX_DATA | MAX_BIDI | MAX_UNI | DATA_BLOCKED | BLOCKED_BIDI | BLOCKED_UNI
    )
}
fn is_stream_count(kind: u64) -> bool {
    matches!(kind, MAX_BIDI | MAX_UNI | BLOCKED_BIDI | BLOCKED_UNI)
}
fn read_varint(bytes: &[u8]) -> u64 {
    bytes[1..]
        .iter()
        .fold(u64::from(bytes[0] & 0x3f), |value, byte| {
            (value << 8) | u64::from(*byte)
        })
}
fn append_varint(output: &mut Vec<u8>, value: u64) {
    let (size, prefix) = match value {
        0..64 => (1, 0),
        64..16_384 => (2, 0x40),
        16_384..1_073_741_824 => (4, 0x80),
        _ => (8, 0xc0),
    };
    let bytes = value.to_be_bytes();
    let start = output.len();
    output.extend_from_slice(&bytes[8 - size..]);
    output[start] |= prefix;
}

/// Encodes a valid UTF-8 close message, truncated at a character boundary.
pub fn encode_close(code: u32, message: &str) -> Vec<u8> {
    let mut end = message.len().min(MAX_CLOSE_MESSAGE);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = Vec::with_capacity(end + 8);
    append_varint(&mut output, CLOSE);
    append_varint(&mut output, (4 + end) as u64);
    output.extend_from_slice(&code.to_be_bytes());
    output.extend_from_slice(&message.as_bytes()[..end]);
    output
}

pub fn encode(capsule: &Capsule) -> Result<Vec<u8>, Error> {
    let (kind, value) = match capsule {
        Capsule::CloseSession { code, message } => {
            return Ok(encode_close(
                *code,
                std::str::from_utf8(message).map_err(|_| Error::InvalidUtf8)?,
            ));
        }
        Capsule::MaxData(value) => (MAX_DATA, *value),
        Capsule::MaxStreamsBidi(value) => (MAX_BIDI, *value),
        Capsule::MaxStreamsUni(value) => (MAX_UNI, *value),
        Capsule::DataBlocked(value) => (DATA_BLOCKED, *value),
        Capsule::StreamsBlockedBidi(value) => (BLOCKED_BIDI, *value),
        Capsule::StreamsBlockedUni(value) => (BLOCKED_UNI, *value),
    };
    if value > MAX_VARINT || (is_stream_count(kind) && value > MAX_STREAMS) {
        return Err(Error::ValueOutOfRange);
    }
    let mut output = Vec::with_capacity(17);
    append_varint(&mut output, kind);
    let size = match value {
        0..64 => 1,
        64..16_384 => 2,
        16_384..1_073_741_824 => 4,
        _ => 8,
    };
    append_varint(&mut output, size);
    append_varint(&mut output, value);
    Ok(output)
}

/// Appends a canonical QUIC variable-length integer, leaving output unchanged on error.
pub fn encode_varint(value: u64, output: &mut Vec<u8>) -> Result<(), Error> {
    if value > MAX_VARINT {
        return Err(Error::ValueOutOfRange);
    }
    append_varint(output, value);
    Ok(())
}

/// Reads a QUIC varint prefix; incomplete input returns `None`. Nonminimal forms are valid.
pub fn decode_varint(input: &[u8]) -> Result<Option<(u64, usize)>, Error> {
    let Some(first) = input.first() else {
        return Ok(None);
    };
    let size = 1 << (first >> 6);
    if input.len() < size {
        return Ok(None);
    }
    Ok(Some((read_varint(&input[..size]), size)))
}
