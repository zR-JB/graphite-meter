//! The peer's control stream after its type (RFC 9114 §6.2.1): SETTINGS first, then GOAWAY,
//! MAX_PUSH_ID and CANCEL_PUSH, read as they arrive in arbitrary chunks.
use crate::{
    code::Code,
    frame::{self, Piece},
    settings::{self, Peer},
};
use bytes::Bytes;

/// What the peer's control stream tells this side.
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
    value: frame::Varint,
    /// The lowest stream ID of the server's GOAWAYs.
    goaway: Option<u64>,
}

impl Reader {
    /// Reads what `input` holds, passing each event on; `client` says this side is the client, so
    /// the peer is a server. An error carries the code the connection closes with.
    pub(crate) fn read(
        &mut self,
        client: bool,
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
                Piece::Header { kind, length } => {
                    self.kind = kind;
                    match kind {
                        frame::MAX_PUSH_ID if client => return Err(Code::H3_FRAME_UNEXPECTED),
                        frame::GOAWAY | frame::MAX_PUSH_ID | frame::CANCEL_PUSH if !(1..=8).contains(&length) => {
                            return Err(Code::H3_FRAME_ERROR);
                        }
                        frame::SETTINGS | frame::DATA | frame::HEADERS | frame::PUSH_PROMISE => {
                            return Err(Code::H3_FRAME_UNEXPECTED);
                        }
                        frame::WEBTRANSPORT_BIDI => return Err(Code::H3_FRAME_ERROR),
                        kind if frame::is_http2(kind) => return Err(Code::H3_FRAME_UNEXPECTED),
                        _ => {}
                    }
                }
                Piece::Payload(mut payload) => {
                    if let Some(settings) = &mut self.settings {
                        if let Some(peer) = settings.read(&mut payload)? {
                            self.settings = None;
                            event(Event::Settings(peer))?;
                        }
                    } else if matches!(self.kind, frame::GOAWAY | frame::MAX_PUSH_ID | frame::CANCEL_PUSH) {
                        let value = self.value.read(&mut payload);
                        // The varint must end exactly where the frame does.
                        if !payload.is_empty() || value.is_some() != (self.frames.remaining() == 0) {
                            return Err(Code::H3_FRAME_ERROR);
                        }
                        // A server's GOAWAY names request streams; a client's names push IDs, and we never push.
                        if let Some(value) = value
                            && self.kind == frame::GOAWAY
                            && client
                        {
                            if !value.is_multiple_of(4) || self.goaway.is_some_and(|previous| value > previous) {
                                return Err(Code::H3_ID_ERROR);
                            }
                            self.goaway = Some(value);
                            event(Event::Goaway(value))?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
