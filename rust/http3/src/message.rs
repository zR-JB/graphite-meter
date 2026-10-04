//! A request stream's frames (RFC 9114 §4.1): HEADERS, DATA, optional trailers, then FIN.
use crate::{
    code::{Code, WtCode},
    fields,
    frame::{self, Piece},
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
    started: bool,
    kind: u64,
    /// A field section split across chunks; its capacity is the frame's declared length.
    section: Vec<u8>,
    limit: u64,
    /// Body bytes the head's content-length still owes.
    owed: Option<u64>,
    /// A response, on which a server could interleave PUSH_PROMISE.
    response: bool,
}

impl Message {
    pub(crate) fn new(limit: u64, response: bool) -> Self {
        let frames = frame::Reader::default();
        let (phase, section) = (Phase::Head, Vec::new());
        Self {
            frames,
            phase,
            started: false,
            kind: 0,
            section,
            limit,
            owed: None,
            response,
        }
    }

    /// Holds a request's body to its head's content-length.
    pub(crate) fn content_length(&mut self, length: Option<u64>) {
        self.owed = length;
    }

    /// Takes a response head: `None` for an interim one, after which the message starts over, else the
    /// final response, whose content its request's method and its status bound.
    pub(crate) fn response(
        &mut self,
        section: &[u8],
        method: &http::Method,
    ) -> Result<Option<http::Response<()>>, Code> {
        let head = fields::decode_response(section, self.limit).map_err(fields::Invalid::code)?;
        let status = head.message.status();
        if status.is_informational() {
            *self = Self::new(self.limit, true);
            return Ok(None);
        }
        // A client ignores content-length in a successful response to CONNECT (RFC 9110 §9.3.6), and
        // these never have content, whatever it says (RFC 9114 §4.1.2).
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

    /// The next event `input` completes. An error carries the code the stream, or for frame
    /// violations the connection, ends with.
    pub(crate) fn next(&mut self, input: &mut Bytes) -> Result<Option<Event>, Code> {
        while let Some(piece) = self.frames.next(input) {
            match piece {
                Piece::Header { kind, length } => {
                    if let Some(event) = self.header(kind, length)? {
                        return Ok(Some(event));
                    }
                }
                Piece::Payload(payload) => {
                    if let Some(event) = self.payload(payload)? {
                        return Ok(Some(event));
                    }
                }
            }
        }
        Ok(None)
    }

    fn header(&mut self, kind: u64, length: u64) -> Result<Option<Event>, Code> {
        // The WebTransport signal may open a client's stream, before any frame.
        let opening = !std::mem::replace(&mut self.started, true) && !self.response;
        self.kind = kind;
        match (kind, self.phase) {
            (frame::HEADERS, Phase::Head | Phase::Body) if length > self.limit => Err(Code::H3_EXCESSIVE_LOAD),
            (frame::HEADERS, Phase::Head | Phase::Body) if length == 0 => Ok(Some(self.fields(Bytes::new()))),
            (frame::HEADERS, Phase::Head | Phase::Body) | (frame::DATA, Phase::Body) => Ok(None),
            (frame::WEBTRANSPORT_BIDI, _) if !opening => Err(Code::H3_FRAME_ERROR),
            (frame::WEBTRANSPORT_BIDI, _) if !length.is_multiple_of(4) => Err(Code::H3_ID_ERROR),
            // No route takes a peer's WebTransport stream: it is refused like a cancelled lane.
            (frame::WEBTRANSPORT_BIDI, _) => Err(WtCode(0).to_http()),
            // We send no MAX_PUSH_ID, so every push ID is over our limit (RFC 9114 §7.2.5).
            (frame::PUSH_PROMISE, _) if self.response => Err(Code::H3_ID_ERROR),
            (
                frame::DATA
                | frame::HEADERS
                | frame::SETTINGS
                | frame::GOAWAY
                | frame::MAX_PUSH_ID
                | frame::CANCEL_PUSH
                | frame::PUSH_PROMISE,
                _,
            ) => Err(Code::H3_FRAME_UNEXPECTED),
            (kind, _) if frame::is_http2(kind) => Err(Code::H3_FRAME_UNEXPECTED),
            _ => Ok(None),
        }
    }

    fn payload(&mut self, payload: Bytes) -> Result<Option<Event>, Code> {
        match self.kind {
            frame::DATA => {
                if let Some(owed) = &mut self.owed {
                    *owed = owed.checked_sub(payload.len() as u64).ok_or(Code::H3_MESSAGE_ERROR)?;
                }
                Ok(Some(Event::Data(payload)))
            }
            frame::HEADERS if self.frames.remaining == 0 && self.section.is_empty() => Ok(Some(self.fields(payload))),
            frame::HEADERS => {
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

    /// Bytes of a field section being assembled across chunks.
    pub(crate) fn buffered(&self) -> usize {
        self.section.capacity()
    }

    /// Whether the head has been read, so a request may be in processing.
    pub(crate) fn has_head(&self) -> bool {
        self.phase != Phase::Head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpack;

    fn frame(kind: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        frame::put_header(kind, payload.len() as u64, &mut bytes);
        [bytes, payload.to_vec()].concat()
    }

    fn head(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut section = Vec::new();
        qpack::encode(fields.iter().map(|(name, value)| (name.as_bytes(), value.as_bytes())), &mut section);
        frame(frame::HEADERS, &section)
    }

    /// Reads `bytes` in chunks of `chunk` bytes, then FIN; the events and how the stream ends.
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
        let (events, end) = read(&mut Message::new(4096, false), bytes, bytes.len().max(1));
        end.map(|()| events)
    }

    #[test]
    fn a_request_reads_the_same_in_any_chunks() {
        let section = head(&[(":method", "POST")]);
        let bytes = [section.clone(), frame(frame::DATA, b"body"), frame(0x21, b"grease"), head(&[("x", "1")])];
        for chunk in [1, 2, 7, 1024] {
            let (events, end) = read(&mut Message::new(4096, false), &bytes.concat(), chunk);
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
            assert!(matches!(events.last(), Some(Event::Trailers(_))), "chunk {chunk}");
        }
    }

    #[test]
    fn frame_decisions_on_a_request_stream() {
        let get = head(&[(":method", "GET")]);
        for (bytes, code) in [
            (frame(frame::DATA, b"body"), Code::H3_FRAME_UNEXPECTED),
            ([get.clone(), head(&[]), head(&[])].concat(), Code::H3_FRAME_UNEXPECTED),
            ([get.clone(), frame(frame::SETTINGS, &[])].concat(), Code::H3_FRAME_UNEXPECTED),
            ([get.clone(), frame(frame::GOAWAY, &[0])].concat(), Code::H3_FRAME_UNEXPECTED),
            ([get.clone(), frame(frame::PUSH_PROMISE, &[0])].concat(), Code::H3_FRAME_UNEXPECTED),
            ([get.clone(), frame(0x02, &[])].concat(), Code::H3_FRAME_UNEXPECTED),
            (frame(frame::HEADERS, &[0; 4097]), Code::H3_EXCESSIVE_LOAD),
            ([get.clone(), frame(frame::WEBTRANSPORT_BIDI, &[])].concat(), Code::H3_FRAME_ERROR),
            ([frame(0x21, &[]), frame(frame::WEBTRANSPORT_BIDI, &[])].concat(), Code::H3_FRAME_ERROR),
            (frame(frame::WEBTRANSPORT_BIDI, &[0; 2]), Code::H3_ID_ERROR),
            (frame(frame::WEBTRANSPORT_BIDI, &[0; 4]), WtCode(0).to_http()),
            (get[..get.len() - 1].to_vec(), Code::H3_FRAME_ERROR),
            (frame(0x21, b"grease"), Code::H3_REQUEST_INCOMPLETE),
        ] {
            assert_eq!(request(&bytes), Err(code), "{bytes:x?}");
        }
        assert_eq!(request(&frame(frame::HEADERS, &[])), Ok(vec![Event::Head(Bytes::new())]));
    }

    #[test]
    fn the_body_matches_its_content_length() {
        let post = head(&[(":method", "POST")]);
        for (body, end) in [(&b"12345"[..], Ok(())), (b"1234", Err(Code::H3_MESSAGE_ERROR))] {
            let mut message = Message::new(4096, false);
            assert!(matches!(message.next(&mut Bytes::from(post.clone())), Ok(Some(Event::Head(_)))));
            message.content_length(Some(5));
            assert_eq!(read(&mut message, &frame(frame::DATA, body), 1).1, end);
        }
        let mut message = Message::new(4096, false);
        message.next(&mut Bytes::from(post)).unwrap();
        message.content_length(Some(5));
        let excess = frame(frame::DATA, b"123456");
        assert_eq!(read(&mut message, &excess, excess.len()).1, Err(Code::H3_MESSAGE_ERROR));
    }

    #[test]
    fn responses_restart_after_interim_heads_and_bound_their_content() {
        let mut message = Message::new(4096, true);
        let push = frame(frame::PUSH_PROMISE, &[0, 0, 0]);
        assert_eq!(read(&mut message, &push, push.len()).1, Err(Code::H3_ID_ERROR));
        let section = |status: &str, length: &str| {
            let mut section = Vec::new();
            qpack::encode(
                [(&b":status"[..], status.as_bytes()), (b"content-length", length.as_bytes())],
                &mut section,
            );
            section
        };
        let mut message = Message::new(4096, true);
        assert!(matches!(message.response(&section("103", "0"), &http::Method::GET), Ok(None)), "interim");
        for (method, status, body, end) in [
            (http::Method::GET, "200", &b"12"[..], Ok(())),
            (http::Method::GET, "200", b"1", Err(Code::H3_MESSAGE_ERROR)),
            (http::Method::GET, "204", b"1", Err(Code::H3_MESSAGE_ERROR)),
            (http::Method::HEAD, "200", b"", Ok(())),
            (http::Method::CONNECT, "200", b"capsules", Ok(())),
        ] {
            let mut message = Message::new(4096, true);
            message
                .next(&mut Bytes::from(frame(frame::HEADERS, &section(status, "2"))))
                .unwrap();
            assert!(message.response(&section(status, "2"), &method).unwrap().is_some());
            let data = frame(frame::DATA, body);
            assert_eq!(read(&mut message, &data, data.len()).1, end, "{method} {status}");
        }
    }
}
