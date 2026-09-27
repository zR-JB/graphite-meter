//! HTTP/3 frames (RFC 9114 §7.2): an incremental reader and header encoding.
use crate::varint;
use bytes::{Buf, BufMut, Bytes};

pub(crate) const DATA: u64 = 0x00;
pub(crate) const HEADERS: u64 = 0x01;
pub(crate) const CANCEL_PUSH: u64 = 0x03;
pub(crate) const SETTINGS: u64 = 0x04;
pub(crate) const PUSH_PROMISE: u64 = 0x05;
pub(crate) const GOAWAY: u64 = 0x07;
pub(crate) const MAX_PUSH_ID: u64 = 0x0d;

/// Frame types reserved for their HTTP/2 meaning; receipt is H3_FRAME_UNEXPECTED.
pub(crate) fn is_http2(kind: u64) -> bool {
    matches!(kind, 0x02 | 0x06 | 0x08 | 0x09)
}

/// Appends a frame header.
pub(crate) fn put_header(kind: u64, length: u64, output: &mut impl BufMut) {
    varint::put(kind, output);
    varint::put(length, output);
}

/// Two consecutive varints, such as a frame header or a setting, split anywhere across chunks.
#[derive(Default)]
pub(crate) struct Pair {
    bytes: [u8; 16],
    used: u8,
}

impl Pair {
    /// Consumes `input` up to the end of the pair and returns it once complete.
    pub(crate) fn read(&mut self, input: &mut impl Buf) -> Option<(u64, u64)> {
        loop {
            let used = usize::from(self.used);
            let need = match self.bytes[..used].first() {
                None => 1,
                Some(&first) => {
                    let first = varint::size(first);
                    self.bytes[..used]
                        .get(first)
                        .map_or(first + 1, |&second| first + varint::size(second))
                }
            };
            if used == need {
                break;
            }
            let take = (need - used).min(input.remaining());
            if take == 0 {
                return None;
            }
            input.copy_to_slice(&mut self.bytes[used..used + take]);
            self.used += take as u8;
        }
        let (first, size) = varint::decode(&self.bytes).expect("complete varint");
        let (second, _) = varint::decode(&self.bytes[size..]).expect("complete varint");
        self.used = 0;
        Some((first, second))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.used == 0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Header {
        kind: u64,
        length: u64,
    },
    /// Part of the current frame's payload, sliced from the input without copying.
    Payload(Bytes),
}

/// Splits a stream into frame headers and payload slices; frame boundaries may fall anywhere.
#[derive(Default)]
pub(crate) struct Reader {
    header: Pair,
    remaining: u64,
}

impl Reader {
    pub(crate) fn next(&mut self, input: &mut Bytes) -> Option<Piece> {
        if self.remaining == 0 {
            let (kind, length) = self.header.read(input)?;
            self.remaining = length;
            return Some(Piece::Header { kind, length });
        }
        if input.is_empty() {
            return None;
        }
        let take = usize::try_from(self.remaining).map_or(input.len(), |remaining| remaining.min(input.len()));
        self.remaining -= take as u64;
        Some(Piece::Payload(input.split_to(take)))
    }

    /// Payload bytes the current frame still owes.
    pub(crate) fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Whether the stream may end here without truncating a frame.
    pub(crate) fn at_boundary(&self) -> bool {
        self.remaining == 0 && self.header.is_empty()
    }
}
