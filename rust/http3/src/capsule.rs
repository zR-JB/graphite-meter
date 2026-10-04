//! HTTP datagrams and capsules (RFC 9297) as WebTransport sessions use them.
use crate::{code::Code, frame, varint};
use bytes::{Buf, BufMut, Bytes};

const CLOSE: u64 = 0x2843;
/// WT_MAX_STREAM_DATA and WT_STREAM_DATA_BLOCKED exist only over HTTP/2.
const HTTP2_ONLY: [u64; 2] = [0x190b4d3e, 0x190b4d42];
pub(crate) const MAX_REASON: usize = 1024;

/// Reads CLOSE capsules, as their code and reason, from a CONNECT stream's DATA. Others, DRAIN and
/// flow control included, are skipped unbuffered.
#[derive(Default)]
pub(crate) struct Reader {
    frames: frame::Reader,
    kind: u64,
    close: Vec<u8>,
}

impl Reader {
    pub(crate) fn read(&mut self, input: &mut Bytes) -> Result<Option<(u32, String)>, Code> {
        while let Some(piece) = self.frames.next(input) {
            match piece {
                frame::Piece::Header { kind, length } => {
                    if HTTP2_ONLY.contains(&kind) || kind == CLOSE && !(4..=4 + MAX_REASON as u64).contains(&length) {
                        return Err(Code::H3_MESSAGE_ERROR);
                    }
                    self.kind = kind;
                }
                frame::Piece::Payload(body) if self.kind == CLOSE => {
                    self.close.extend_from_slice(&body);
                    if self.frames.remaining == 0 {
                        let mut close = std::mem::take(&mut self.close);
                        let reason = String::from_utf8(close.split_off(4)).map_err(|_| Code::H3_MESSAGE_ERROR)?;
                        let code = u32::from_be_bytes(close.try_into().expect("four code bytes"));
                        return Ok(Some((code, reason)));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// Whether the stream may end here without truncating a capsule.
    pub(crate) fn at_boundary(&self) -> bool {
        self.frames.at_boundary()
    }
}

/// A CLOSE_WEBTRANSPORT_SESSION capsule; the reason is cut at a character boundary within 1024 bytes.
pub(crate) fn close(code: u32, reason: &str) -> Vec<u8> {
    let mut end = reason.len().min(MAX_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let mut capsule = Vec::with_capacity(end + 12);
    frame::put_header(CLOSE, 4 + end as u64, &mut capsule);
    capsule.put_u32(code);
    capsule.extend_from_slice(&reason.as_bytes()[..end]);
    capsule
}

/// Splits an HTTP datagram into its session, the CONNECT stream's ID, and its payload.
pub(crate) fn datagram(mut payload: Bytes) -> Result<(u64, Bytes), Code> {
    match varint::decode(&payload) {
        Some((quarter, size)) if quarter < 1 << 60 => {
            payload.advance(size);
            Ok((quarter * 4, payload))
        }
        _ => Err(Code::H3_DATAGRAM_ERROR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capsule(kind: u64, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        frame::put_header(kind, body.len() as u64, &mut bytes);
        [bytes, body.to_vec()].concat()
    }

    fn read_all(chunks: impl IntoIterator<Item = Vec<u8>>) -> Result<(Vec<(u32, String)>, bool), Code> {
        let mut reader = Reader::default();
        let mut capsules = Vec::new();
        for chunk in chunks {
            let mut chunk = Bytes::from(chunk);
            while let Some(capsule) = reader.read(&mut chunk)? {
                capsules.push(capsule);
            }
            assert!(chunk.is_empty());
        }
        Ok((capsules, reader.at_boundary()))
    }

    #[test]
    fn every_split_skips_flow_control_grease_and_drain() {
        let mut bytes = capsule(0x190b4d3d, &[0x80, 0, 0, 1]);
        bytes.extend(capsule(0x17 + 41 * 3, b"grease"));
        bytes.extend(capsule(0x78ae, b""));
        bytes.extend(close(0xf123_4567, "done"));
        let expected = vec![(0xf123_4567, "done".into())];
        for split in 0..=bytes.len() {
            let (first, second) = bytes.split_at(split);
            assert_eq!(read_all([first.to_vec(), second.to_vec()]), Ok((expected.clone(), true)), "split {split}");
        }
        assert_eq!(read_all(bytes.iter().map(|&byte| vec![byte])), Ok((expected, true)));
    }

    #[test]
    fn unknown_bodies_are_not_retained() {
        let mut header = Vec::new();
        frame::put_header(0x21, varint::MAX, &mut header);
        let mut reader = Reader::default();
        assert_eq!(reader.read(&mut Bytes::from(header)), Ok(None));
        for _ in 0..64 {
            assert_eq!(reader.read(&mut Bytes::from(vec![0; 4096])), Ok(None));
        }
        assert_eq!((reader.close.capacity(), reader.at_boundary()), (0, false));
    }

    #[test]
    fn invalid_close_and_http2_capsules_end_the_session() {
        let long = [b'a'; MAX_REASON + 1];
        for bytes in [
            capsule(CLOSE, &[0; 3]),
            capsule(CLOSE, &[&[0; 4][..], &long].concat()),
            capsule(CLOSE, &[0, 0, 0, 1, 0xff]),
            capsule(0x190b4d3e, &[0]),
            capsule(0x190b4d42, &[0]),
        ] {
            assert_eq!(read_all([bytes]), Err(Code::H3_MESSAGE_ERROR));
        }
        let text = format!("{}€tail", "a".repeat(MAX_REASON - 1));
        assert_eq!(read_all([close(1, &text)]), Ok((vec![(1, "a".repeat(MAX_REASON - 1))], true)));
        let bytes = close(7, "bye");
        for end in 1..bytes.len() {
            assert_eq!(read_all([bytes[..end].to_vec()]), Ok((vec![], false)), "end {end}");
        }
    }

    #[test]
    fn datagrams_route_by_quarter_stream_id() {
        for quarter in [0, 1, 63, 64, 16384, (1 << 60) - 1] {
            let mut bytes = Vec::new();
            varint::put(quarter, &mut bytes);
            bytes.extend_from_slice(b"PING,42");
            assert_eq!(datagram(bytes.into()), Ok((quarter * 4, Bytes::from_static(b"PING,42"))));
        }
        for invalid in [&b""[..], b"\x40", b"\xd0\x00\x00\x00\x00\x00\x00\x00"] {
            assert_eq!(datagram(Bytes::from_static(invalid)), Err(Code::H3_DATAGRAM_ERROR));
        }
    }
}
