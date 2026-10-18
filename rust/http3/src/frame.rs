//! Reads HTTP/3 frame and stream types (RFC 9114 §6.2, §7.2) incrementally from chunks of any size.
use crate::varint::{self, Partial};
use bytes::{Buf, BufMut, Bytes};
use std::marker::PhantomData;

/// A type leads a frame, a capsule or a unidirectional stream, each with its own space of varint codes.
pub(crate) trait Kind: Copy {
    fn from_wire(code: u64) -> Self;
    fn wire(self) -> u64;
}

/// Identifies a frame type (RFC 9114 §7.2). Peers may send ones not named here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameType(pub(crate) u64);

impl FrameType {
    pub(crate) const DATA: Self = Self(0x00);
    pub(crate) const HEADERS: Self = Self(0x01);
    pub(crate) const CANCEL_PUSH: Self = Self(0x03);
    pub(crate) const SETTINGS: Self = Self(0x04);
    pub(crate) const PUSH_PROMISE: Self = Self(0x05);
    pub(crate) const GOAWAY: Self = Self(0x07);
    pub(crate) const MAX_PUSH_ID: Self = Self(0x0d);
    /// Opens a peer's WebTransport bidirectional stream in place of a frame type. Elsewhere it is H3_FRAME_ERROR.
    pub(crate) const WEBTRANSPORT_BIDI: Self = Self(0x41);

    /// The type is reserved for its HTTP/2 meaning, and receiving it is H3_FRAME_UNEXPECTED.
    pub(crate) fn is_http2(self) -> bool {
        matches!(self.0, 0x02 | 0x06 | 0x08 | 0x09)
    }
}

impl Kind for FrameType {
    fn from_wire(code: u64) -> Self {
        Self(code)
    }

    fn wire(self) -> u64 {
        self.0
    }
}

/// Identifies a unidirectional stream type (RFC 9114 §6.2). Peers may open ones not named here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreamType(pub(crate) u64);

impl StreamType {
    pub(crate) const CONTROL: Self = Self(0x00);
    pub(crate) const PUSH: Self = Self(0x01);
    pub(crate) const ENCODER: Self = Self(0x02);
    pub(crate) const DECODER: Self = Self(0x03);
    pub(crate) const WEBTRANSPORT: Self = Self(0x54);
}

impl Kind for StreamType {
    fn from_wire(code: u64) -> Self {
        Self(code)
    }

    fn wire(self) -> u64 {
        self.0
    }
}

pub(crate) fn put_header(kind: impl Kind, length: u64, output: &mut impl BufMut) {
    varint::put(kind.wire(), output);
    varint::put(length, output);
}

/// Writes two varints, such as a frame header, a stream header or a setting, in as many parts as it takes.
#[derive(Default)]
pub(crate) struct Header {
    bytes: [u8; 16],
    end: u8,
    written: u8,
}

impl Header {
    /// Lays out `kind`, then a frame's length, or the session ID after a WebTransport stream's type.
    pub(crate) fn new(kind: impl Kind, second: u64) -> Self {
        let mut bytes = [0; 16];
        let mut rest = &mut bytes[..];
        put_header(kind, second, &mut rest);
        let end = (16 - rest.len()) as u8;
        Self {
            bytes,
            end,
            written: 0,
        }
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

/// Reads two varints in sequence.
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

    /// Reads a unidirectional stream's type, followed by a session ID on WebTransport streams.
    pub(crate) fn stream_type(
        &mut self,
        input: &mut impl Buf,
    ) -> Option<(StreamType, Option<u64>)> {
        match StreamType(self.first.map_or_else(|| self.varint.read(input), Some)?) {
            StreamType::WEBTRANSPORT => {
                self.first = Some(StreamType::WEBTRANSPORT.0);
                self.read(input)
                    .map(|(_, session)| (StreamType::WEBTRANSPORT, Some(session)))
            }
            kind => Some((kind, None)),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Piece<K> {
    Header {
        kind: K,
        length: u64,
    },
    /// Holds part of the current frame's payload, sliced from the input without copying.
    Payload(Bytes),
}

/// Splits a stream into headers of `K`, frame or capsule types, and payload slices.
#[derive(Default)]
pub(crate) struct Reader<K> {
    header: Pair,
    /// The current frame still owes this many payload bytes.
    pub(crate) remaining: u64,
    kind: PhantomData<K>,
}

impl<K: Kind> Reader<K> {
    pub(crate) fn next(&mut self, input: &mut Bytes) -> Option<Piece<K>> {
        if self.remaining == 0 {
            let (kind, length) = self.header.read(input)?;
            self.remaining = length;
            return Some(Piece::Header {
                kind: K::from_wire(kind),
                length,
            });
        }
        if input.is_empty() {
            return None;
        }
        let take = usize::try_from(self.remaining)
            .map_or(input.len(), |remaining| remaining.min(input.len()));
        self.remaining -= take as u64;
        Some(Piece::Payload(input.split_to(take)))
    }

    /// Reports whether the stream may end here without truncating a frame.
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
        let grease = FrameType(0x21 + 0x1f * 1000);
        let expected = [
            (FrameType::HEADERS, &b"head"[..]),
            (FrameType::DATA, b""),
            (grease, b"grease"),
            (FrameType::DATA, b"x"),
        ];
        for (kind, payload) in expected {
            put_header(kind, payload.len() as u64, &mut stream);
            stream.extend_from_slice(payload);
        }
        for split in 0..=stream.len() {
            let mut reader = Reader::<FrameType>::default();
            let mut frames = Vec::new();
            for part in [&stream[..split], &stream[split..]] {
                let mut part = Bytes::copy_from_slice(part);
                while let Some(piece) = reader.next(&mut part) {
                    match piece {
                        Piece::Header { kind, .. } => frames.push((kind, Vec::new())),
                        Piece::Payload(bytes) => {
                            frames.last_mut().unwrap().1.extend_from_slice(&bytes);
                        }
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
        let mut reader = Reader::<FrameType>::default();
        reader.next(&mut Bytes::from_static(&[0x00, 0x02, 0x61]));
        assert!(!reader.at_boundary(), "a truncated payload");
    }
}
