//! The peer's control stream after its type (RFC 9114 §6.2.1): SETTINGS first, then GOAWAY,
//! MAX_PUSH_ID and CANCEL_PUSH, read as they arrive in arbitrary chunks.
use crate::{
    code::Code,
    frame::{self, Piece},
    settings::{self, Peer},
    varint,
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
    value: [u8; 8],
    used: usize,
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
                    self.used = 0;
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
                        self.value[self.used..self.used + payload.len()].copy_from_slice(&payload);
                        self.used += payload.len();
                        if self.frames.remaining() == 0 {
                            let value = match varint::decode(&self.value[..self.used]) {
                                Some((value, size)) if size == self.used => value,
                                _ => return Err(Code::H3_FRAME_ERROR),
                            };
                            // A server's GOAWAY names request streams; a client's names push IDs, and we never push.
                            if self.kind == frame::GOAWAY && client {
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
        }
        Ok(())
    }
}
