//! Request streams over noq: payload passes through as the chunks noq charged, framed without a copy.
use crate::{
    charge::{Budget, Charge},
    code::Code,
    connection::{Role, Shared},
    error::Error,
    fields, frame,
    message::{Event, Message},
    qpack::Invalid,
};
use bytes::Bytes;
use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Waker, ready},
};

/// A request stream: the server receives one from [`crate::server::Request::resolve`], the client
/// from [`crate::client::SendRequest::send_request`].
pub struct RequestStream {
    pub(crate) send: SendHalf,
    pub(crate) recv: RecvHalf,
}

/// Charges for a request stream's two halves, so each can drop on its own.
pub(crate) fn charges(budget: &Budget) -> Option<(Charge, Charge)> {
    Some((
        Charge::new(budget, size_of::<SendHalf>())?,
        Charge::new(budget, size_of::<RecvHalf>())?,
    ))
}

impl RequestStream {
    pub(crate) fn new(
        shared: &Arc<Shared>,
        send: noq::SendStream,
        recv: noq::RecvStream,
        limit: u64,
        (send_charge, recv_charge): (Charge, Charge),
    ) -> Self {
        shared.hold(2);
        Self {
            send: SendHalf::new(shared.clone(), send, send_charge),
            recv: RecvHalf {
                stream: recv,
                message: Message::new(limit, shared.role == Role::Client),
                input: Bytes::new(),
                shared: shared.clone(),
                charge: recv_charge,
                head: false,
                done: false,
            },
        }
    }

    pub fn split(self) -> (SendHalf, RecvHalf) {
        (self.send, self.recv)
    }

    /// The QUIC stream ID; a CONNECT stream's is its WebTransport session ID.
    pub fn id(&self) -> u64 {
        self.send.stream.id().into()
    }

    /// Answers `code` in both directions.
    pub(crate) fn abort(mut self, code: Code) {
        self.recv.stop(code);
        self.send.reset(code);
    }

    pub(crate) fn recv_abort(&mut self, code: Code) -> Error {
        self.send.reset(code);
        self.recv.abort(code)
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.recv.shared
    }

    pub(crate) fn shared_arc(&self) -> Arc<Shared> {
        self.recv.shared.clone()
    }
}

/// Reads a message: its head, then DATA payloads as noq delivered them. Trailers are checked and dropped.
pub struct RecvHalf {
    stream: noq::RecvStream,
    message: Message,
    input: Bytes,
    shared: Arc<Shared>,
    charge: Charge,
    /// A response to HEAD has no body.
    pub(crate) head: bool,
    /// FIN arrived, or the stream was stopped.
    done: bool,
}

impl RecvHalf {
    pub(crate) fn poll_head(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Error>> {
        match ready!(self.poll_event(cx))? {
            Some(Event::Head(section)) => Poll::Ready(Ok(section)),
            _ => Poll::Ready(Err(self.abort(Code::H3_REQUEST_INCOMPLETE))),
        }
    }

    pub fn poll_data(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Bytes>, Error>> {
        loop {
            match ready!(self.poll_event(cx))? {
                Some(Event::Data(data)) => return Poll::Ready(Ok(Some(data))),
                Some(Event::Trailers(section)) => {
                    if let Err(invalid) = fields::check_trailers(&section, self.shared.role.field_limit()) {
                        return Poll::Ready(Err(self.abort(invalid_code(invalid))));
                    }
                }
                Some(Event::Head(_)) => return Poll::Ready(Err(self.abort(Code::H3_FRAME_UNEXPECTED))),
                None => return Poll::Ready(Ok(None)),
            }
        }
    }

    /// The next DATA payload, or `None` once the message is complete.
    pub async fn data(&mut self) -> Result<Option<Bytes>, Error> {
        poll_fn(|cx| self.poll_data(cx)).await
    }

    /// Reads a response head, skipping interim responses.
    pub async fn response(&mut self) -> Result<http::Response<()>, Error> {
        loop {
            let section = poll_fn(|cx| self.poll_head(cx)).await?;
            match fields::decode_response(&section, self.shared.role.field_limit()) {
                Ok(response) if response.message.status().is_informational() => {
                    self.message = Message::new(self.shared.role.field_limit(), true);
                }
                Ok(response) => {
                    self.message
                        .content_length(if self.head { Some(0) } else { response.content_length });
                    return Ok(response.message);
                }
                Err(invalid) => return Err(self.abort(invalid_code(invalid))),
            }
        }
    }

    pub fn stop(&mut self, code: Code) {
        if !std::mem::replace(&mut self.done, true) {
            let _ = self.stream.stop(code.into());
        }
    }

    pub fn id(&self) -> u64 {
        self.stream.id().into()
    }

    pub(crate) fn content_length(&mut self, length: Option<u64>) {
        self.message.content_length(length);
    }

    fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Event>, Error>> {
        loop {
            let event = self.message.next(&mut self.input);
            if !self.charge.resize(size_of::<Self>() + self.message.buffered()) {
                return Poll::Ready(Err(self.abort(Code::H3_EXCESSIVE_LOAD)));
            }
            match event {
                Err(code) => return Poll::Ready(Err(self.abort(code))),
                Ok(Some(event)) => return Poll::Ready(Ok(Some(event))),
                Ok(None) => {}
            }
            let chunk = ready!(pin!(self.stream.read_chunk(usize::MAX)).poll(cx));
            match chunk {
                Ok(Some(chunk)) => self.input = chunk,
                Ok(None) => {
                    self.done = true;
                    return Poll::Ready(self.message.finish().map(|()| None).map_err(|code| self.abort(code)));
                }
                Err(error) => {
                    self.done = true;
                    return Poll::Ready(Err(error.into()));
                }
            }
        }
    }

    /// Frame, ID and QPACK violations close the connection; the rest end only this stream.
    fn abort(&mut self, code: Code) -> Error {
        if matches!(
            code,
            Code::H3_FRAME_UNEXPECTED | Code::H3_FRAME_ERROR | Code::H3_ID_ERROR | Code::QPACK_DECOMPRESSION_FAILED
        ) {
            return self.shared.close(code);
        }
        self.stop(code);
        Error::Protocol(code)
    }
}

impl Drop for RecvHalf {
    fn drop(&mut self) {
        // A route may ignore a bodyless request, so look for FIN at the last moment: stopping a
        // finished stream makes some clients report a reset after a complete response.
        if !self.done && self.input.is_empty() && self.message.finish().is_ok() {
            let mut cx = Context::from_waker(Waker::noop());
            self.done = matches!(
                pin!(self.stream.read_chunk(usize::MAX)).poll(&mut cx),
                Poll::Ready(Ok(None))
            );
        }
        self.stop(match self.shared.role {
            Role::Server => Code::H3_NO_ERROR,
            Role::Client => Code::H3_REQUEST_CANCELLED,
        });
        self.shared.release();
    }
}

fn invalid_code(invalid: Invalid) -> Code {
    match invalid {
        Invalid::Qpack => Code::QPACK_DECOMPRESSION_FAILED,
        Invalid::TooLarge => Code::H3_EXCESSIVE_LOAD,
        Invalid::Malformed | Invalid::Unsupported => Code::H3_MESSAGE_ERROR,
    }
}

/// Writes frames; a partly written frame stays owned here, and dropping it resets instead of FIN.
pub struct SendHalf {
    stream: noq::SendStream,
    header: [u8; 16],
    header_end: u8,
    header_written: u8,
    payload: Bytes,
    shared: Arc<Shared>,
    _charge: Charge,
    finished: bool,
}

impl SendHalf {
    fn new(shared: Arc<Shared>, stream: noq::SendStream, charge: Charge) -> Self {
        Self {
            stream,
            header: [0; 16],
            header_end: 0,
            header_written: 0,
            payload: Bytes::new(),
            shared,
            _charge: charge,
            finished: false,
        }
    }

    /// Writes whatever frame is pending.
    pub fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        while self.header_written < self.header_end {
            let header = &self.header[usize::from(self.header_written)..usize::from(self.header_end)];
            let written = ready!(pin!(self.stream.write(header)).poll(cx))?;
            self.header_written += written as u8;
        }
        while !self.payload.is_empty() {
            let length = self.payload.len();
            let mut chunks = std::slice::from_mut(&mut self.payload);
            // noq keeps an unwritten suffix in place, so a pending write loses nothing.
            if ready!(pin!(self.stream.write_many_chunks(&mut chunks)).poll(cx))? == length {
                self.payload = Bytes::new();
            }
        }
        Poll::Ready(Ok(()))
    }

    /// Queues a frame; the previous one must be written.
    pub(crate) fn queue(&mut self, kind: u64, payload: Bytes) {
        let mut header = &mut self.header[..];
        frame::put_header(kind, payload.len() as u64, &mut header);
        let remaining = header.len();
        (self.header_end, self.header_written, self.payload) = ((16 - remaining) as u8, 0, payload);
    }

    async fn frame(&mut self, kind: u64, payload: Bytes) -> Result<(), Error> {
        poll_fn(|cx| self.poll_ready(cx)).await?;
        self.queue(kind, payload);
        poll_fn(|cx| self.poll_ready(cx)).await
    }

    pub async fn send_data(&mut self, data: Bytes) -> Result<(), Error> {
        self.frame(frame::DATA, data).await
    }

    /// Queues a response head within the peer's field section limit.
    pub(crate) fn queue_response(&mut self, response: http::Response<()>) -> Result<(), Error> {
        let (parts, ()) = response.into_parts();
        let section = fields::encode_response(&parts, self.shared.peer_field_limit()).map_err(|_| Error::Refused)?;
        self.queue(frame::HEADERS, section.into());
        Ok(())
    }

    pub async fn send_response(&mut self, response: http::Response<()>) -> Result<(), Error> {
        poll_fn(|cx| self.poll_ready(cx)).await?;
        self.queue_response(response)?;
        poll_fn(|cx| self.poll_ready(cx)).await
    }

    pub(crate) async fn send_request(&mut self, head: Vec<u8>) -> Result<(), Error> {
        self.frame(frame::HEADERS, head.into()).await
    }

    /// Ends the stream once pending frames are written; a stream that already ended stays so.
    pub(crate) fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if !self.finished {
            let written = ready!(self.poll_ready(cx));
            self.finished = true;
            written?;
            self.stream
                .finish()
                .map_err(|_| Error::Stopped(Code::H3_REQUEST_CANCELLED))?;
        }
        Poll::Ready(Ok(()))
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        poll_fn(|cx| self.poll_finish(cx)).await
    }

    pub fn reset(&mut self, code: Code) {
        if !std::mem::replace(&mut self.finished, true) {
            let _ = self.stream.reset(code.into());
        }
    }

    pub fn id(&self) -> u64 {
        self.stream.id().into()
    }
}

impl Drop for SendHalf {
    fn drop(&mut self) {
        self.reset(Code::H3_REQUEST_CANCELLED);
        self.shared.release();
    }
}
