//! HTTP/3 frames and stream types (RFC 9114 §6.2, §7.2), read incrementally from arbitrary chunks.
use crate::varint;
use bytes::{Buf, BufMut, Bytes};

pub(crate) const DATA: u64 = 0x00;
pub(crate) const HEADERS: u64 = 0x01;
pub(crate) const CANCEL_PUSH: u64 = 0x03;
pub(crate) const SETTINGS: u64 = 0x04;
pub(crate) const PUSH_PROMISE: u64 = 0x05;
pub(crate) const GOAWAY: u64 = 0x07;
pub(crate) const MAX_PUSH_ID: u64 = 0x0d;
/// Opens a peer-initiated WebTransport bidirectional stream in place of a frame type.
pub(crate) const WEBTRANSPORT_BIDI: u64 = 0x41;

pub(crate) const CONTROL_STREAM: u64 = 0x00;
pub(crate) const PUSH_STREAM: u64 = 0x01;
pub(crate) const ENCODER_STREAM: u64 = 0x02;
pub(crate) const DECODER_STREAM: u64 = 0x03;
pub(crate) const WEBTRANSPORT_STREAM: u64 = 0x54;

/// Frame types reserved for their HTTP/2 meaning; receipt is H3_FRAME_UNEXPECTED.
pub(crate) fn is_http2(kind: u64) -> bool {
    matches!(kind, 0x02 | 0x06 | 0x08 | 0x09)
}

pub(crate) fn put_header(kind: u64, length: u64, output: &mut impl BufMut) {
    varint::put(kind, output);
    varint::put(length, output);
}

/// One varint, split anywhere across chunks.
#[derive(Default)]
pub(crate) struct Varint {
    bytes: [u8; 8],
    used: u8,
}

impl Varint {
    pub(crate) fn read(&mut self, input: &mut impl Buf) -> Option<u64> {
        loop {
            let used = usize::from(self.used);
            let need = if used == 0 { 1 } else { varint::size(self.bytes[0]) };
            if used == need {
                self.used = 0;
                return varint::decode(&self.bytes[..used]).map(|(value, _)| value);
            }
            let take = (need - used).min(input.remaining());
            if take == 0 {
                return None;
            }
            input.copy_to_slice(&mut self.bytes[used..used + take]);
            self.used += take as u8;
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.used == 0
    }
}

/// Two varints, such as a frame header or a setting.
#[derive(Default)]
pub(crate) struct Pair {
    first: Option<u64>,
    varint: Varint,
}

impl Pair {
    pub(crate) fn read(&mut self, input: &mut impl Buf) -> Option<(u64, u64)> {
        let first = match self.first {
            Some(first) => first,
            None => *self.first.insert(self.varint.read(input)?),
        };
        let second = self.varint.read(input)?;
        self.first = None;
        Some((first, second))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.first.is_none() && self.varint.is_empty()
    }
}

/// A unidirectional stream's type, followed by a session ID on WebTransport streams.
#[derive(Default)]
pub(crate) struct StreamType {
    kind: Option<u64>,
    varint: Varint,
}

impl StreamType {
    pub(crate) fn read(&mut self, input: &mut impl Buf) -> Option<(u64, Option<u64>)> {
        let kind = match self.kind {
            Some(kind) => kind,
            None => *self.kind.insert(self.varint.read(input)?),
        };
        if kind != WEBTRANSPORT_STREAM {
            return Some((kind, None));
        }
        Some((kind, Some(self.varint.read(input)?)))
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

/// Splits a stream into frame headers and payload slices.
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
