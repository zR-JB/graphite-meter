//! The peer's control stream after its type (RFC 9114 §6.2.1): SETTINGS first, then GOAWAY,
//! MAX_PUSH_ID and CANCEL_PUSH.
use crate::{
    code::Code,
    frame::{self, Piece},
    settings::{self, Peer},
    varint::Partial,
};
use bytes::Bytes;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Settings(Peer),
    /// The server's GOAWAY: requests from this stream ID on go unprocessed.
    Goaway(u64),
}

#[derive(Default)]
pub(crate) struct Reader {
    frames: frame::Reader,
    settings: Option<settings::Reader>,
    started: bool,
    kind: u64,
    /// The one varint a GOAWAY, MAX_PUSH_ID or CANCEL_PUSH frame holds.
    value: Partial,
    goaway: Option<u64>,
}

impl Reader {
    /// Reads `input`, passing each event on; `server` says the peer is a server. An error carries the
    /// code the connection closes with.
    pub(crate) fn read(
        &mut self,
        server: bool,
        input: &mut Bytes,
        mut event: impl FnMut(Event) -> Result<(), Code>,
    ) -> Result<(), Code> {
        while let Some(piece) = self.frames.next(input) {
            match piece {
                Piece::Header { kind, length } if !self.started => {
                    if kind != frame::SETTINGS {
                        return Err(Code::H3_MISSING_SETTINGS);
                    }
                    self.started = true;
                    let mut settings = settings::Reader::new(length)?;
                    match settings.read(&mut Bytes::new())? {
                        Some(peer) => event(Event::Settings(peer))?,
                        None => self.settings = Some(settings),
                    }
                }
                Piece::Header { kind, length } => self.header(kind, length, server)?,
                Piece::Payload(mut payload) => {
                    if let Some(settings) = &mut self.settings {
                        if let Some(peer) = settings.read(&mut payload)? {
                            self.settings = None;
                            event(Event::Settings(peer))?;
                        }
                    } else if let Some(goaway) = self.value(&mut payload, server)? {
                        event(Event::Goaway(goaway))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn header(&mut self, kind: u64, length: u64, server: bool) -> Result<(), Code> {
        self.kind = kind;
        match kind {
            frame::MAX_PUSH_ID if server => Err(Code::H3_FRAME_UNEXPECTED),
            frame::GOAWAY | frame::MAX_PUSH_ID | frame::CANCEL_PUSH if !(1..=8).contains(&length) => {
                Err(Code::H3_FRAME_ERROR)
            }
            frame::SETTINGS | frame::DATA | frame::HEADERS | frame::PUSH_PROMISE => Err(Code::H3_FRAME_UNEXPECTED),
            frame::WEBTRANSPORT_BIDI => Err(Code::H3_FRAME_ERROR),
            kind if frame::is_http2(kind) => Err(Code::H3_FRAME_UNEXPECTED),
            _ => Ok(()),
        }
    }

    /// Reads a one-varint frame's payload; returns a server's GOAWAY once it is complete.
    fn value(&mut self, payload: &mut Bytes, server: bool) -> Result<Option<u64>, Code> {
        if !matches!(self.kind, frame::GOAWAY | frame::MAX_PUSH_ID | frame::CANCEL_PUSH) {
            return Ok(None);
        }
        let value = self.value.read(payload);
        // The varint must end exactly where the frame does.
        if !payload.is_empty() || value.is_some() != (self.frames.remaining == 0) {
            return Err(Code::H3_FRAME_ERROR);
        }
        // A client's GOAWAY names push IDs, and we never push.
        let Some(id) = value.filter(|_| self.kind == frame::GOAWAY && server) else {
            return Ok(None);
        };
        if !id.is_multiple_of(4) || self.goaway.is_some_and(|previous| id > previous) {
            return Err(Code::H3_ID_ERROR);
        }
        self.goaway = Some(id);
        Ok(Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::frame;

    /// The events of a stream that opens with empty SETTINGS, read one byte at a time.
    fn events(server: bool, frames: &[Vec<u8>]) -> Result<Vec<Event>, Code> {
        let mut reader = Reader::default();
        let mut events = Vec::new();
        for byte in [frame(frame::SETTINGS, &[]), frames.concat()].concat() {
            reader.read(server, &mut Bytes::from(vec![byte]), |event| {
                events.push(event);
                Ok(())
            })?;
        }
        Ok(events)
    }

    #[test]
    fn settings_come_first_and_goaway_ids_only_fall() {
        let settings = Event::Settings(Peer::default());
        let goaway = |id| frame(frame::GOAWAY, &[id]);
        assert_eq!(
            events(true, &[goaway(8), goaway(4)]),
            Ok(vec![settings, Event::Goaway(8), Event::Goaway(4)])
        );
        assert_eq!(events(true, &[goaway(4), goaway(8)]), Err(Code::H3_ID_ERROR));
        assert_eq!(events(true, &[goaway(2)]), Err(Code::H3_ID_ERROR));
        assert_eq!(events(false, &[goaway(3)]), Ok(vec![settings]), "a client's GOAWAY names push IDs");
        let mut reader = Reader::default();
        let missing = reader.read(true, &mut Bytes::from(goaway(4)), |_| Ok(()));
        assert_eq!(missing, Err(Code::H3_MISSING_SETTINGS));
    }

    #[test]
    fn frames_that_never_belong_on_a_control_stream() {
        for (server, frames, code) in [
            (true, frame(frame::SETTINGS, &[]), Code::H3_FRAME_UNEXPECTED),
            (true, frame(frame::DATA, &[]), Code::H3_FRAME_UNEXPECTED),
            (true, frame(frame::MAX_PUSH_ID, &[0]), Code::H3_FRAME_UNEXPECTED),
            (true, frame(frame::WEBTRANSPORT_BIDI, &[]), Code::H3_FRAME_ERROR),
            (false, frame(frame::GOAWAY, &[]), Code::H3_FRAME_ERROR),
        ] {
            assert_eq!(events(server, std::slice::from_ref(&frames)), Err(code), "{frames:x?}");
        }
        assert!(events(false, &[frame(frame::MAX_PUSH_ID, &[0]), frame(0x21, b"grease")]).is_ok());
    }
}
