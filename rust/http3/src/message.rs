//! A request stream's frames (RFC 9114 §4.1): HEADERS, DATA, optional trailers, then FIN.
use crate::{
    code::{Code, WtCode},
    fields,
    frame::{self, Piece},
    qpack::Invalid,
};
use bytes::Bytes;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Head(Bytes),
    Data(Bytes),
    Trailers(Bytes),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Head,
    Body,
    Trailers,
}

pub(crate) struct Message {
    frames: frame::Reader,
    phase: Phase,
    kind: u64,
    /// A field section split across chunks; its capacity is the frame's declared length.
    section: Vec<u8>,
    limit: u64,
    owed: Option<u64>,
    /// A response, where a server may interleave PUSH_PROMISE.
    response: bool,
}

impl Message {
    pub(crate) fn new(limit: u64, response: bool) -> Self {
        Self {
            frames: frame::Reader::default(),
            phase: Phase::Head,
            kind: 0,
            section: Vec::new(),
            limit,
            owed: None,
            response,
        }
    }

    /// Holds the body to the head's content-length.
    pub(crate) fn content_length(&mut self, length: Option<u64>) {
        self.owed = length;
    }

    /// Takes a response head: `None` for an interim one, after which the message starts over, else
    /// the final response, whose content its request's method and its status bound.
    pub(crate) fn response(
        &mut self,
        section: &[u8],
        method: &http::Method,
    ) -> Result<Option<http::Response<()>>, Code> {
        let head = fields::decode_response(section, self.limit).map_err(Invalid::code)?;
        let status = head.message.status();
        if status.is_informational() {
            *self = Self::new(self.limit, true);
            return Ok(None);
        }
        // A client ignores content-length in a successful response to CONNECT (RFC 9110 §9.3.6),
        // and these never have content, whatever it says (RFC 9114 §4.1.2).
        self.owed = if *method == http::Method::CONNECT && status.is_success() {
            None
        } else if *method == http::Method::HEAD
            || matches!(status, http::StatusCode::NO_CONTENT | http::StatusCode::NOT_MODIFIED)
        {
            Some(0)
        } else {
            head.content_length
        };
        Ok(Some(head.message))
    }

    /// Errors carry the code the stream, or for frame violations the connection, ends with.
    pub(crate) fn next(&mut self, input: &mut Bytes) -> Result<Option<Event>, Code> {
        while let Some(piece) = self.frames.next(input) {
            match piece {
                Piece::Header { kind, length } => {
                    self.kind = kind;
                    match (kind, self.phase) {
                        (frame::HEADERS, Phase::Head | Phase::Body) if length > self.limit => {
                            return Err(Code::H3_EXCESSIVE_LOAD);
                        }
                        (frame::HEADERS, Phase::Head | Phase::Body) if length == 0 => {
                            return Ok(Some(self.fields(Bytes::new())));
                        }
                        (frame::HEADERS, Phase::Head | Phase::Body) | (frame::DATA, Phase::Body) => {}
                        // No route takes a peer's WebTransport stream: refuse it like a cancelled lane.
                        (frame::WEBTRANSPORT_BIDI, Phase::Head) => return Err(WtCode(0).to_http()),
                        // We send no MAX_PUSH_ID, so every push ID is over our limit (RFC 9114 §7.2.5).
                        (frame::PUSH_PROMISE, _) if self.response => return Err(Code::H3_ID_ERROR),
                        (
                            frame::DATA
                            | frame::HEADERS
                            | frame::SETTINGS
                            | frame::GOAWAY
                            | frame::MAX_PUSH_ID
                            | frame::CANCEL_PUSH
                            | frame::PUSH_PROMISE,
                            _,
                        ) => return Err(Code::H3_FRAME_UNEXPECTED),
                        (kind, _) if frame::is_http2(kind) => return Err(Code::H3_FRAME_UNEXPECTED),
                        _ => {}
                    }
                }
                Piece::Payload(payload) => match self.kind {
                    frame::DATA => {
                        if let Some(owed) = &mut self.owed {
                            *owed = owed.checked_sub(payload.len() as u64).ok_or(Code::H3_MESSAGE_ERROR)?;
                        }
                        return Ok(Some(Event::Data(payload)));
                    }
                    frame::HEADERS if self.frames.remaining() == 0 && self.section.is_empty() => {
                        return Ok(Some(self.fields(payload)));
                    }
                    frame::HEADERS => {
                        if self.section.is_empty() {
                            self.section
                                .reserve_exact(payload.len() + self.frames.remaining() as usize);
                        }
                        self.section.extend_from_slice(&payload);
                        if self.frames.remaining() == 0 {
                            let section = std::mem::take(&mut self.section);
                            return Ok(Some(self.fields(section.into())));
                        }
                    }
                    _ => {}
                },
            }
        }
        Ok(None)
    }

    fn fields(&mut self, section: Bytes) -> Event {
        if self.phase == Phase::Head {
            self.phase = Phase::Body;
            Event::Head(section)
        } else {
            self.phase = Phase::Trailers;
            Event::Trailers(section)
        }
    }

    /// At FIN the message must hold a head, no partial frame and exactly its declared body.
    pub(crate) fn finish(&self) -> Result<(), Code> {
        if !self.frames.at_boundary() {
            Err(Code::H3_FRAME_ERROR)
        } else if self.phase == Phase::Head {
            Err(Code::H3_REQUEST_INCOMPLETE)
        } else if self.owed.is_some_and(|owed| owed > 0) {
            Err(Code::H3_MESSAGE_ERROR)
        } else {
            Ok(())
        }
    }

    /// Bytes of a field section being assembled across chunks.
    pub(crate) fn buffered(&self) -> usize {
        self.section.capacity()
    }

    /// Whether the head has been read, so a request may be in processing.
    pub(crate) fn has_head(&self) -> bool {
        self.phase != Phase::Head
    }
}
