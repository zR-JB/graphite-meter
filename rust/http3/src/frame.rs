//! HTTP/3 frame and stream types (RFC 9114 §6.2, §7.2), read incrementally from chunks of any size.
use crate::varint::{self, Partial};
use bytes::{Buf, BufMut, Bytes};

pub(crate) const DATA: u64 = 0x00;
pub(crate) const HEADERS: u64 = 0x01;
pub(crate) const CANCEL_PUSH: u64 = 0x03;
pub(crate) const SETTINGS: u64 = 0x04;
pub(crate) const PUSH_PROMISE: u64 = 0x05;
pub(crate) const GOAWAY: u64 = 0x07;
pub(crate) const MAX_PUSH_ID: u64 = 0x0d;
/// Opens a peer's WebTransport bidirectional stream in place of a frame type; elsewhere it is H3_FRAME_ERROR.
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

/// Two varints, such as a frame header, a stream header or a setting, written in as many parts as it takes.
#[derive(Default)]
pub(crate) struct Header {
    bytes: [u8; 16],
    end: u8,
    written: u8,
}

impl Header {
    pub(crate) fn new(first: u64, second: u64) -> Self {
        let mut bytes = [0; 16];
        let mut rest = &mut bytes[..];
        put_header(first, second, &mut rest);
        let end = (16 - rest.len()) as u8;
        Self { bytes, end, written: 0 }
    }

    pub(crate) fn len(&self) -> u8 {
        self.end
    }

    /// The bytes not yet written.
    pub(crate) fn rest(&self) -> &[u8] {
        &self.bytes[usize::from(self.written)..usize::from(self.end)]
    }

    pub(crate) fn advance(&mut self, written: usize) {
        self.written += written as u8;
    }

    pub(crate) fn is_written(&self) -> bool {
        self.written == self.end
    }
}

/// Two varints read in sequence.
#[derive(Default)]
pub(crate) struct Pair {
    first: Option<u64>,
    varint: Partial,
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
    varint: Partial,
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
    /// Payload bytes the current frame still owes.
    pub(crate) remaining: u64,
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

    /// Whether the stream may end here without truncating a frame.
    pub(crate) fn at_boundary(&self) -> bool {
        self.remaining == 0 && self.header.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_read_the_same_from_every_split() {
        let mut stream = Vec::new();
        for (kind, payload) in [(HEADERS, &b"head"[..]), (DATA, b""), (0x21 + 0x1f * 1000, b"grease"), (DATA, b"x")] {
            put_header(kind, payload.len() as u64, &mut stream);
            stream.extend_from_slice(payload);
        }
        let expected = [(HEADERS, &b"head"[..]), (DATA, b""), (0x21 + 0x1f * 1000, b"grease"), (DATA, b"x")];
        for split in 0..=stream.len() {
            let mut reader = Reader::default();
            let mut frames = Vec::new();
            for part in [&stream[..split], &stream[split..]] {
                let mut part = Bytes::copy_from_slice(part);
                while let Some(piece) = reader.next(&mut part) {
                    match piece {
                        Piece::Header { kind, .. } => frames.push((kind, Vec::new())),
                        Piece::Payload(bytes) => frames.last_mut().unwrap().1.extend_from_slice(&bytes),
                    }
                }
                assert!(part.is_empty());
            }
            let frames: Vec<_> = frames
                .iter()
                .map(|(kind, payload)| (*kind, payload.as_slice()))
                .collect();
            assert_eq!(frames, expected, "split {split}");
            assert!(reader.at_boundary());
        }
        let mut reader = Reader::default();
        reader.next(&mut Bytes::from_static(&[0x00, 0x02, 0x61]));
        assert!(!reader.at_boundary(), "a truncated payload");
    }
}
