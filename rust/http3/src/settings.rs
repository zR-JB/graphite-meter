//! SETTINGS (RFC 9114 §7.2.4): our profile per role, the peer's values and its WebTransport dialect.
use crate::{code::Code, frame, varint};
use bytes::{Buf, BufMut};

const MAX_FIELD_SECTION_SIZE: u64 = 0x06;
const ENABLE_CONNECT_PROTOCOL: u64 = 0x08;
const H3_DATAGRAM: u64 = 0x33;
const H3_DATAGRAM_DRAFT04: u64 = 0xffd277;
const WT_ENABLE_DRAFT02: u64 = 0x2b603742;
const WT_MAX_SESSIONS_DRAFT07: u64 = 0xc671706a;
const WT_MAX_SESSIONS_DRAFT13: u64 = 0x14e9cd29;
const WT_ENABLED: u64 = 0x2c7cf000;
/// Go's bound on a SETTINGS frame.
const MAX_LENGTH: u64 = 8 * 1024;
/// Our bound on a frame's distinct identifiers; Go has none.
const MAX_IDS: usize = 64;

/// The largest field section each role accepts: Go's server limit, and our client's.
pub(crate) const SERVER_FIELD_SECTION: u64 = 4096;
pub(crate) const CLIENT_FIELD_SECTION: u64 = 32 * 1024;

/// Exactly the Go server's set: static-only QPACK and no WebTransport flow control.
pub(crate) const SERVER: [(u64, u64); 6] = [
    (MAX_FIELD_SECTION_SIZE, SERVER_FIELD_SECTION),
    (ENABLE_CONNECT_PROTOCOL, 1),
    (H3_DATAGRAM, 1),
    (WT_ENABLE_DRAFT02, 1),
    (WT_ENABLED, 1),
    (WT_MAX_SESSIONS_DRAFT13, varint::MAX),
];
pub(crate) const CLIENT: [(u64, u64); 3] =
    [(MAX_FIELD_SECTION_SIZE, CLIENT_FIELD_SECTION), (H3_DATAGRAM, 1), (WT_ENABLED, 1)];

/// A control stream's first bytes: its stream type, then the SETTINGS frame.
pub(crate) fn control_stream(settings: &[(u64, u64)]) -> Vec<u8> {
    let length: usize = settings
        .iter()
        .map(|&(id, value)| varint::len(id) + varint::len(value))
        .sum();
    let mut bytes = Vec::with_capacity(length + 9);
    bytes.put_u8(frame::CONTROL_STREAM as u8);
    frame::put_header(frame::SETTINGS, length as u64, &mut bytes);
    for &(id, value) in settings {
        varint::put(id, &mut bytes);
        varint::put(value, &mut bytes);
    }
    bytes
}

/// How a peer speaks WebTransport: draft 02 (Chromium and Firefox) or the current draft (Go).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Draft02,
    Draft15,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Peer {
    pub(crate) max_field_section_size: Option<u64>,
    pub(crate) connect_protocol: bool,
    /// RFC 9297's H3_DATAGRAM, as opposed to draft 04's.
    h3_datagram: bool,
    datagrams: bool,
    draft02: bool,
    current: bool,
}

impl Peer {
    /// RFC 9297 §2.1.1: H3_DATAGRAM without the datagram transport parameter is H3_SETTINGS_ERROR, as
    /// Go has it; draft 04's setting alone only leaves WebTransport off.
    pub(crate) fn check(&self, datagram_frames: bool) -> Result<(), Code> {
        if self.h3_datagram && !datagram_frames {
            Err(Code::H3_SETTINGS_ERROR)
        } else {
            Ok(())
        }
    }

    /// WebTransport needs a WebTransport signal, HTTP datagrams and the datagram transport parameter.
    /// Peers on drafts 07 to 14 get the current dialect without flow control, as with Go.
    pub(crate) fn webtransport(&self, datagram_frames: bool) -> Option<Dialect> {
        if !self.datagrams || !datagram_frames {
            None
        } else if self.current {
            Some(Dialect::Draft15)
        } else {
            self.draft02.then_some(Dialect::Draft02)
        }
    }
}

/// Parses a SETTINGS frame's payload as it arrives, without buffering it.
pub(crate) struct Reader {
    pair: frame::Pair,
    remaining: u64,
    seen: [u64; MAX_IDS],
    count: usize,
    peer: Peer,
}

impl Reader {
    pub(crate) fn new(length: u64) -> Result<Self, Code> {
        if length > MAX_LENGTH {
            return Err(Code::H3_EXCESSIVE_LOAD);
        }
        let pair = frame::Pair::default();
        Ok(Self {
            pair,
            remaining: length,
            seen: [0; MAX_IDS],
            count: 0,
            peer: Peer::default(),
        })
    }

    /// Takes the next part of the payload; returns the settings once the frame is complete.
    pub(crate) fn read(&mut self, payload: &mut impl Buf) -> Result<Option<Peer>, Code> {
        self.remaining -= payload.remaining() as u64;
        while let Some((id, value)) = self.pair.read(payload) {
            self.apply(id, value)?;
        }
        match (self.remaining, self.pair.is_empty()) {
            (0, true) => Ok(Some(self.peer)),
            (0, false) => Err(Code::H3_FRAME_ERROR),
            _ => Ok(None),
        }
    }

    fn apply(&mut self, id: u64, value: u64) -> Result<(), Code> {
        // HTTP/2's identifiers are reserved; duplicates are refused, as by Go.
        if matches!(id, 0x00 | 0x02..=0x05) || self.seen[..self.count].contains(&id) {
            return Err(Code::H3_SETTINGS_ERROR);
        }
        if self.count == MAX_IDS {
            return Err(Code::H3_EXCESSIVE_LOAD);
        }
        self.seen[self.count] = id;
        self.count += 1;
        let peer = &mut self.peer;
        match id {
            MAX_FIELD_SECTION_SIZE => peer.max_field_section_size = Some(value),
            ENABLE_CONNECT_PROTOCOL | H3_DATAGRAM if value > 1 => return Err(Code::H3_SETTINGS_ERROR),
            ENABLE_CONNECT_PROTOCOL => peer.connect_protocol = value == 1,
            H3_DATAGRAM => {
                peer.h3_datagram = value == 1;
                peer.datagrams |= peer.h3_datagram;
            }
            H3_DATAGRAM_DRAFT04 => peer.datagrams |= value == 1,
            WT_ENABLE_DRAFT02 => peer.draft02 = value == 1,
            WT_ENABLED | WT_MAX_SESSIONS_DRAFT13 | WT_MAX_SESSIONS_DRAFT07 => peer.current |= value > 0,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREASE: u64 = 0xde67c359b;

    /// Parses `settings` one payload byte at a time, so every setting straddles reads.
    fn parse(settings: &[(u64, u64)]) -> Result<Peer, Code> {
        let stream = control_stream(settings);
        let (kind, length) = frame::Pair::default().read(&mut &stream[1..]).unwrap();
        assert_eq!(kind, frame::SETTINGS);
        let payload = &stream[stream.len() - length as usize..];
        let mut reader = Reader::new(length)?;
        let mut peer = if payload.is_empty() { reader.read(&mut &payload[..])? } else { None };
        for byte in payload.chunks(1) {
            assert!(peer.is_none());
            peer = reader.read(&mut &byte[..])?;
        }
        Ok(peer.expect("a complete frame"))
    }

    #[test]
    fn measured_peers_pick_their_dialect() {
        let chromium = [
            (0x01, 65536),
            (0x07, 100),
            (0x06, 16384),
            (0x33, 1),
            (0xffd277, 1),
            (0x2b603742, 1),
            (GREASE, 1),
        ];
        let firefox = [(0x01, 65536), (0x07, 20), (0x2b603742, 1), (0xffd277, 1), (0x33, 1), (0x08, 1)];
        let go_client = [(0x06, 10 << 20), (0x33, 1), (0x2c7cf000, 1)];
        for (settings, dialect) in [
            (&chromium[..], Dialect::Draft02),
            (&firefox, Dialect::Draft02),
            (&go_client, Dialect::Draft15),
            (&SERVER, Dialect::Draft15),
            (&CLIENT, Dialect::Draft15),
        ] {
            let peer = parse(settings).unwrap();
            assert_eq!(peer.webtransport(true), Some(dialect), "{settings:x?}");
            assert_eq!(peer.webtransport(false), None, "no datagram transport parameter");
        }
        let server = parse(&SERVER).unwrap();
        assert_eq!((server.max_field_section_size, server.connect_protocol), (Some(4096), true));
        assert_eq!(parse(&CLIENT).unwrap().max_field_section_size, Some(32 * 1024));
        assert_eq!(parse(&firefox).unwrap().max_field_section_size, None);
    }

    #[test]
    fn synthetic_peers() {
        for (settings, dialect) in [
            (&[(0xc671706a, 1), (0x33, 1)][..], Some(Dialect::Draft15)),
            (&[(0x2b603742, 1), (0xffd277, 1)], Some(Dialect::Draft02)),
            (&[(0x2b603742, 1)], None),
            (&[(0x2b603742, 2), (0x33, 1)], None),
            (&[(0x2c7cf000, 0), (0x2b603742, 1), (0x33, 1)], Some(Dialect::Draft02)),
            (&[], None),
        ] {
            assert_eq!(parse(settings).unwrap().webtransport(true), dialect, "{settings:x?}");
        }
        for (settings, datagram_frames, checked) in [
            (&[(0x33, 1)][..], false, Err(Code::H3_SETTINGS_ERROR)),
            (&[(0x33, 1)], true, Ok(())),
            (&[(0x33, 0)], false, Ok(())),
        ] {
            assert_eq!(parse(settings).unwrap().check(datagram_frames), checked, "{settings:x?}");
        }
    }

    #[test]
    fn refusals() {
        for invalid in [&[(0x08, 2)][..], &[(0x33, 2)], &[(0x02, 0)], &[(0x00, 0)], &[(GREASE, 1), (GREASE, 2)]] {
            assert_eq!(parse(invalid), Err(Code::H3_SETTINGS_ERROR), "{invalid:x?}");
        }
        let many: Vec<_> = (0..=MAX_IDS as u64).map(|index| (0x21 + 0x1f * index, 0)).collect();
        assert_eq!(parse(&many), Err(Code::H3_EXCESSIVE_LOAD));
        assert!(parse(&many[1..]).is_ok());
        assert_eq!(Reader::new(MAX_LENGTH + 1).err(), Some(Code::H3_EXCESSIVE_LOAD));
        assert!(Reader::new(MAX_LENGTH).is_ok());
        let mut reader = Reader::new(3).unwrap();
        let peer = Peer { max_field_section_size: Some(0), ..Peer::default() };
        assert_eq!(reader.read(&mut &[0x06, 0x40, 0x00][..]), Ok(Some(peer)));
        let mut reader = Reader::new(3).unwrap();
        assert_eq!(reader.read(&mut &[0x06, 0x80, 0x00][..]), Err(Code::H3_FRAME_ERROR), "a truncated value");
    }
}
