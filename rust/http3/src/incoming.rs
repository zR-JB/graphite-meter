//! The peer's unidirectional streams: classified within 10 s, then read as control or QPACK, or handed to WebTransport.
use crate::{
    budget::{Budget, Charge},
    code::Code,
    control,
    driver::{Role, Shared},
    frame, qpack,
};
use bytes::Bytes;
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::Instant;

const TYPE_TIMEOUT: Duration = Duration::from_secs(10);
/// Streams covered by the connection floor, where peers open their critical ones.
const FLOOR: u8 = 3;

pub(crate) struct Incoming {
    streams: Vec<PeerStream>,
    /// Whether the peer opened its control, QPACK encoder and QPACK decoder streams.
    critical: [bool; 3],
    floor: u8,
}

/// A WebTransport stream once its header is read: its session, the stream and the bytes after the header.
pub(crate) type Classified = (u64, noq::RecvStream, Bytes);

pub(crate) struct PeerStream {
    stream: noq::RecvStream,
    input: Bytes,
    kind: Kind,
    _charge: Option<Charge>,
}

enum Kind {
    Unknown { header: frame::StreamType, deadline: Instant },
    Control(Box<control::Reader>),
    Encoder,
    Decoder(qpack::DecoderStream),
    Session(u64),
}

/// What reading a stream came to.
enum Read {
    Pending,
    Done,
    Session(u64),
}

impl Incoming {
    pub(crate) fn new() -> Self {
        Self { streams: Vec::new(), critical: [false; 3], floor: FLOOR }
    }

    /// A budget refusal stops the new stream, never a critical one within the floor.
    pub(crate) fn admit(&mut self, mut stream: noq::RecvStream, budget: &Budget) {
        let charge = match self.floor.checked_sub(1) {
            Some(floor) => {
                self.floor = floor;
                None
            }
            None => match Charge::new(budget, size_of::<PeerStream>()) {
                Some(charge) => Some(charge),
                None => return drop(stream.stop(Code::H3_REQUEST_REJECTED.into())),
            },
        };
        let kind = Kind::Unknown {
            header: frame::StreamType::default(),
            deadline: Instant::now() + TYPE_TIMEOUT,
        };
        self.streams
            .push(PeerStream { stream, input: Bytes::new(), kind, _charge: charge });
    }

    /// Reads every stream as far as it goes; `Ready` with a WebTransport stream or a violation's close code.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, shared: &Shared) -> Poll<Result<Classified, Code>> {
        let mut index = 0;
        while index < self.streams.len() {
            match self.streams[index].read(cx, shared, &mut self.critical)? {
                Read::Pending => index += 1,
                Read::Done => drop(self.streams.swap_remove(index)),
                Read::Session(session) => {
                    let PeerStream { stream, input, .. } = self.streams.swap_remove(index);
                    return Poll::Ready(Ok((session, stream, input)));
                }
            }
        }
        Poll::Pending
    }

    /// Stops streams that did not declare their type in time.
    pub(crate) fn expire(&mut self, now: Instant) {
        self.streams.retain_mut(|peer| match peer.kind {
            Kind::Unknown { deadline, .. } if deadline <= now => {
                let _ = peer.stream.stop(Code::H3_STREAM_CREATION_ERROR.into());
                false
            }
            _ => true,
        });
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        let deadlines = self.streams.iter().filter_map(|peer| match peer.kind {
            Kind::Unknown { deadline, .. } => Some(deadline),
            _ => None,
        });
        deadlines.min()
    }
}

impl PeerStream {
    fn read(&mut self, cx: &mut Context<'_>, shared: &Shared, critical: &mut [bool; 3]) -> Result<Read, Code> {
        loop {
            match &mut self.kind {
                Kind::Unknown { header, .. } => {
                    if let Some((kind, session)) = header.read(&mut self.input) {
                        let Some(kind) = classify(kind, session, shared.role, critical)? else {
                            let _ = self.stream.stop(Code::H3_STREAM_CREATION_ERROR.into());
                            return Ok(Read::Done);
                        };
                        self.kind = kind;
                        continue;
                    }
                }
                Kind::Control(reader) => {
                    reader.read(shared.role == Role::Client, &mut self.input, |event| shared.control(event))?
                }
                Kind::Encoder => qpack::encoder_stream(&std::mem::take(&mut self.input))?,
                Kind::Decoder(decoder) => decoder.read(&std::mem::take(&mut self.input))?,
                Kind::Session(session) => return Ok(Read::Session(*session)),
            }
            match pin!(self.stream.read_chunk(usize::MAX)).poll(cx) {
                Poll::Pending => return Ok(Read::Pending),
                Poll::Ready(Ok(Some(chunk))) => self.input = chunk,
                Poll::Ready(Err(noq::ReadError::ConnectionLost(_))) => return Ok(Read::Pending),
                Poll::Ready(_) if matches!(self.kind, Kind::Unknown { .. }) => return Ok(Read::Done),
                Poll::Ready(_) => return Err(Code::H3_CLOSED_CRITICAL_STREAM),
            }
        }
    }
}

/// The kind a stream's type makes it, or `None` for an unknown type, which is stopped.
fn classify(kind: u64, session: Option<u64>, role: Role, critical: &mut [bool; 3]) -> Result<Option<Kind>, Code> {
    if let Some(session) = session {
        // Only a client-initiated bidirectional stream can carry a session.
        return if session.is_multiple_of(4) {
            Ok(Some(Kind::Session(session)))
        } else {
            Err(Code::H3_ID_ERROR)
        };
    }
    let (slot, next) = match kind {
        frame::CONTROL_STREAM => (0, Kind::Control(Box::default())),
        frame::ENCODER_STREAM => (1, Kind::Encoder),
        frame::DECODER_STREAM => (2, Kind::Decoder(qpack::DecoderStream::default())),
        frame::PUSH_STREAM if role == Role::Client => return Err(Code::H3_ID_ERROR),
        frame::PUSH_STREAM => return Err(Code::H3_STREAM_CREATION_ERROR),
        _ => return Ok(None),
    };
    if std::mem::replace(&mut critical[slot], true) {
        return Err(Code::H3_STREAM_CREATION_ERROR);
    }
    Ok(Some(next))
}
