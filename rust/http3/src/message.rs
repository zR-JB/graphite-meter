//! Reads a request stream's frames (RFC 9114 §4.1), HEADERS, DATA, optional trailers, then FIN.
use crate::{
    code::Code,
    driver::Role,
    fields,
    frame::{FrameType, Piece, Reader},
    qpack,
};
use bytes::Bytes;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Head(Bytes),
    Data(Bytes),
    Trailers(Bytes),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Head,
    Body,
    Trailers,
}

#[derive(Default)]
pub(crate) struct Message {
    frames: Reader<FrameType>,
    phase: Phase,
    started: bool,
    kind: FrameType,
    /// Collects a field section split across chunks. Its capacity is the frame's declared length.
    section: Vec<u8>,
    limit: u64,
    /// The head's content-length still owes this many body bytes.
    pub(crate) owed: Option<u64>,
    /// The message is a response, on which a server could interleave PUSH_PROMISE.
    response: bool,
}

impl Message {
    /// Reads a message `peer` sends. A server sends responses.
    pub(crate) fn new(limit: u64, peer: Role) -> Self {
        Self { limit, response: peer == Role::Server, ..Self::default() }
    }

    /// Takes a response head. An interim one yields `None` and restarts the message. A final one is bounded by status.
    pub(crate) fn response(
        &mut self,
        section: &[u8],
        method: &http::Method,
    ) -> Result<Option<http::Response<()>>, Code> {
        let head = fields::decode_response(section, self.limit).map_err(qpack::Invalid::code)?;
        let status = head.message.status();
        if status.is_informational() {
            *self = Self::new(self.limit, Role::Server);
            return Ok(None);
        }
        // A successful CONNECT ignores content-length (RFC 9110 §9.3.6); these have no content (RFC 9114 §4.1.2).
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

    /// Reads the next event from `input`. Errors carry the code ending the stream, or the connection for frame errors.
    pub(crate) fn next(&mut self, input: &mut Bytes) -> Result<Option<Event>, Code> {
        while let Some(piece) = self.frames.next(input) {
            let event = match piece {
                Piece::Header { kind, length } => self.header(kind, length)?,
                Piece::Payload(payload) => self.payload(payload)?,
            };
            if event.is_some() {
                return Ok(event);
            }
        }
        Ok(None)
    }

    fn header(&mut self, kind: FrameType, length: u64) -> Result<Option<Event>, Code> {
        // The WebTransport signal may open a client's stream, before any frame.
        let opening = !std::mem::replace(&mut self.started, true) && !self.response;
        self.kind = kind;
        match (kind, self.phase) {
            (FrameType::HEADERS, Phase::Head | Phase::Body) if length > self.limit => Err(Code::H3_EXCESSIVE_LOAD),
            (FrameType::HEADERS, Phase::Head | Phase::Body) if length == 0 => Ok(Some(self.fields(Bytes::new()))),
            (FrameType::HEADERS, Phase::Head | Phase::Body) | (FrameType::DATA, Phase::Body) => Ok(None),
            (FrameType::WEBTRANSPORT_BIDI, _) if !opening => Err(Code::H3_FRAME_ERROR),
            (FrameType::WEBTRANSPORT_BIDI, _) if !length.is_multiple_of(4) => Err(Code::H3_ID_ERROR),
            // No route takes a peer's WebTransport stream, so it is refused like a cancelled lane.
            (FrameType::WEBTRANSPORT_BIDI, _) => Err(Code::WT_CANCELLED),
            // We send no MAX_PUSH_ID, so every push ID is over our limit (RFC 9114 §7.2.5).
            (FrameType::PUSH_PROMISE, _) if self.response => Err(Code::H3_ID_ERROR),
            (
                FrameType::DATA
                | FrameType::HEADERS
                | FrameType::SETTINGS
                | FrameType::GOAWAY
                | FrameType::MAX_PUSH_ID
                | FrameType::CANCEL_PUSH
                | FrameType::PUSH_PROMISE,
                _,
            ) => Err(Code::H3_FRAME_UNEXPECTED),
            (kind, _) if kind.is_http2() => Err(Code::H3_FRAME_UNEXPECTED),
            _ => Ok(None),
        }
    }

    fn payload(&mut self, payload: Bytes) -> Result<Option<Event>, Code> {
        match self.kind {
            FrameType::DATA => {
                if let Some(owed) = &mut self.owed {
                    *owed = owed.checked_sub(payload.len() as u64).ok_or(Code::H3_MESSAGE_ERROR)?;
                }
                Ok(Some(Event::Data(payload)))
            }
            FrameType::HEADERS if self.frames.remaining == 0 && self.section.is_empty() => {
                Ok(Some(self.fields(payload)))
            }
            FrameType::HEADERS => {
                if self.section.is_empty() {
                    self.section
                        .reserve_exact(payload.len() + self.frames.remaining as usize);
                }
                self.section.extend_from_slice(&payload);
                if self.frames.remaining > 0 {
                    return Ok(None);
                }
                let section = std::mem::take(&mut self.section);
                Ok(Some(self.fields(section.into())))
            }
            _ => Ok(None),
        }
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

    /// Counts the bytes of a field section being assembled across chunks.
    pub(crate) fn buffered(&self) -> usize {
        self.section.capacity()
    }

    /// Reports whether the head has been read, so a request may be in processing.
    pub(crate) fn has_head(&self) -> bool {
        self.phase != Phase::Head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{frame, section};
    use std::assert_matches;

    fn head(fields: &[(&str, &str)]) -> Vec<u8> {
        frame(FrameType::HEADERS, &section(fields))
    }

    /// Reads `bytes` in chunks of `chunk` bytes, then FIN, and returns the events and how the stream ends.
    fn read(message: &mut Message, bytes: &[u8], chunk: usize) -> (Vec<Event>, Result<(), Code>) {
        let mut events = Vec::new();
        for part in bytes.chunks(chunk) {
            let mut part = Bytes::copy_from_slice(part);
            loop {
                match message.next(&mut part) {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(code) => return (events, Err(code)),
                }
            }
        }
        let finished = message.finish();
        (events, finished)
    }

    fn request(bytes: &[u8]) -> Result<Vec<Event>, Code> {
        let (events, end) = read(&mut Message::new(4096, Role::Client), bytes, bytes.len().max(1));
        end.map(|()| events)
    }

    #[test]
    fn a_request_reads_the_same_in_any_chunks() {
        let section = head(&[(":method", "POST")]);
        let bytes = [
            section.clone(),
            frame(FrameType::DATA, b"body"),
            frame(FrameType(0x21), b"grease"),
            head(&[("x", "1")]),
        ];
        for chunk in [1, 2, 7, 1024] {
            let (events, end) = read(&mut Message::new(4096, Role::Client), &bytes.concat(), chunk);
            assert_eq!(end, Ok(()));
            let data: Vec<u8> = events
                .iter()
                .flat_map(|event| match event {
                    Event::Data(data) => data.to_vec(),
                    _ => Vec::new(),
                })
                .collect();
            assert_eq!(events.first(), Some(&Event::Head(section[2..].to_vec().into())));
            assert_eq!(data, b"body");
            assert_matches!(events.last(), Some(Event::Trailers(_)), "chunk {chunk}");
        }
    }

    #[test]
    fn frame_decisions_on_a_request_stream() {
        let get = head(&[(":method", "GET")]);
        for (bytes, code) in [
            (frame(FrameType::DATA, b"body"), Code::H3_FRAME_UNEXPECTED),
            ([get.clone(), head(&[]), head(&[])].concat(), Code::H3_FRAME_UNEXPECTED),
            ([get.clone(), frame(FrameType::PUSH_PROMISE, &[0])].concat(), Code::H3_FRAME_UNEXPECTED),
            (frame(FrameType::HEADERS, &[0; 4097]), Code::H3_EXCESSIVE_LOAD),
            ([get.clone(), frame(FrameType::WEBTRANSPORT_BIDI, &[])].concat(), Code::H3_FRAME_ERROR),
            (frame(FrameType::WEBTRANSPORT_BIDI, &[0; 2]), Code::H3_ID_ERROR),
            (frame(FrameType::WEBTRANSPORT_BIDI, &[0; 4]), Code::WT_CANCELLED),
            (get[..get.len() - 1].to_vec(), Code::H3_FRAME_ERROR),
            (frame(FrameType(0x21), b"grease"), Code::H3_REQUEST_INCOMPLETE),
        ] {
            assert_eq!(request(&bytes), Err(code), "{bytes:x?}");
        }
        assert_eq!(request(&frame(FrameType::HEADERS, &[])), Ok(vec![Event::Head(Bytes::new())]));
    }

    #[test]
    fn responses_restart_after_interim_heads_and_bound_their_content() {
        let mut message = Message::new(4096, Role::Server);
        let push = frame(FrameType::PUSH_PROMISE, &[0, 0, 0]);
        assert_eq!(read(&mut message, &push, push.len()).1, Err(Code::H3_ID_ERROR));
        let section = |status| section(&[(":status", status), ("content-length", "2")]);
        let mut message = Message::new(4096, Role::Server);
        assert_matches!(message.response(&section("103"), &http::Method::GET), Ok(None), "interim");
        for (method, status, body, end) in [
            (http::Method::GET, "200", &b"12"[..], Ok(())),
            (http::Method::GET, "200", b"1", Err(Code::H3_MESSAGE_ERROR)),
            (http::Method::HEAD, "200", b"", Ok(())),
            (http::Method::CONNECT, "200", b"capsules", Ok(())),
        ] {
            let mut message = Message::new(4096, Role::Server);
            message
                .next(&mut Bytes::from(frame(FrameType::HEADERS, &section(status))))
                .unwrap();
            assert!(message.response(&section(status), &method).unwrap().is_some());
            let data = frame(FrameType::DATA, body);
            assert_eq!(read(&mut message, &data, data.len()).1, end, "{method} {status}");
        }
    }
}
