//! The connection driver for both roles: our control stream, the peer's control and QPACK streams,
//! stream classification, GOAWAY and idle. Streams hold [`Shared`], the state they need from it.
use crate::{
    charge::{Budget, Charge},
    code::Code,
    error::Error,
    frame::{self, Piece},
    settings::{self, Peer},
    stream::{self, RequestStream},
    varint,
};
use bytes::Bytes;
use std::{
    future::Future,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker, ready},
    time::Duration,
};
use tokio::{
    sync::watch,
    time::{Instant, Sleep},
};

const TYPE_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE: Duration = Duration::from_secs(15);
const DRAIN: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Server,
    Client,
}

impl Role {
    /// The largest field section this side accepts, as its SETTINGS say.
    pub(crate) fn field_limit(self) -> u64 {
        match self {
            Self::Server => settings::SERVER_FIELD_SECTION,
            Self::Client => settings::CLIENT_FIELD_SECTION,
        }
    }
}

pub(crate) struct Shared {
    pub(crate) quic: noq::Connection,
    pub(crate) budget: Budget,
    pub(crate) role: Role,
    pub(crate) peer: watch::Sender<Option<Peer>>,
    state: Mutex<State>,
}

struct State {
    /// Request stream halves alive; a server connection without any for a while closes.
    live: usize,
    idle_since: Instant,
    driver: Option<Waker>,
    /// The lowest stream ID of the server's GOAWAYs.
    goaway: Option<u64>,
    /// The code this side closed the connection with.
    closed: Option<Code>,
}

impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("HTTP/3 state poisoned")
    }

    pub(crate) fn hold(&self, halves: usize) {
        self.state().live += halves;
    }

    pub(crate) fn release(&self) {
        let mut state = self.state();
        state.live -= 1;
        if state.live == 0 {
            state.idle_since = Instant::now();
            if let Some(driver) = state.driver.take() {
                driver.wake();
            }
        }
    }

    pub(crate) fn peer_field_limit(&self) -> Option<u64> {
        self.peer.borrow().and_then(|peer| peer.max_field_section_size)
    }

    pub(crate) fn going_away(&self) -> bool {
        self.state().goaway.is_some()
    }

    /// Closes the connection, on a protocol violation or with H3_NO_ERROR when it is done. noq would
    /// report any earlier close, the peer's included, as local, so that one stands.
    pub(crate) fn close(&self, code: Code) -> Error {
        if self.quic.close_reason().is_none() {
            self.state().closed.get_or_insert(code);
            self.quic.close(code.into(), b"");
        }
        Error::Connection {
            local: true,
            code,
            reason: Bytes::new(),
        }
    }
}

type Pending<T> = Pin<Box<dyn Future<Output = Result<T, noq::ConnectionError>> + Send>>;

/// The layer's fixed state per connection, for the application's connection floor: the driver,
/// what its streams share, the peer's control and QPACK streams, and three boxed stream futures.
pub const CONNECTION_BYTES: usize =
    size_of::<Connection>() + size_of::<Shared>() + 3 * size_of::<Uni>() + size_of::<PeerControl>() + 3 * 256;

fn accept_uni(quic: &noq::Connection) -> Pending<noq::RecvStream> {
    let quic = quic.clone();
    Box::pin(async move { quic.accept_uni().await })
}

fn accept_bi(quic: &noq::Connection) -> Pending<(noq::SendStream, noq::RecvStream)> {
    let quic = quic.clone();
    Box::pin(async move { quic.accept_bi().await })
}

pub(crate) struct Connection {
    pub(crate) shared: Arc<Shared>,
    control: Control,
    uni: Pending<noq::RecvStream>,
    bi: Option<Pending<(noq::SendStream, noq::RecvStream)>>,
    streams: Vec<Uni>,
    /// Whether the peer opened its control, QPACK encoder and QPACK decoder streams.
    critical: [bool; 3],
    /// Streams still covered by [`CONNECTION_BYTES`] instead of a charge.
    floor: u8,
    /// Server: the lowest request stream ID not yet accepted, and the GOAWAY drain deadline.
    next_request: u64,
    drain: Option<Instant>,
    timer: Pin<Box<Sleep>>,
    closed: bool,
}

/// Our control stream: SETTINGS first, GOAWAY on shutdown.
struct Control {
    opening: Option<Pending<noq::SendStream>>,
    stream: Option<noq::SendStream>,
    pending: Vec<u8>,
    written: usize,
}

struct Uni {
    stream: noq::RecvStream,
    input: Bytes,
    kind: Kind,
    /// The first three streams, where peers open their critical ones, are part of the floor.
    _charge: Option<Charge>,
}

enum Kind {
    Unknown {
        header: frame::StreamType,
        deadline: Instant,
    },
    Control(Box<PeerControl>),
    Encoder,
    Decoder {
        continuing: bool,
    },
}

#[derive(Default)]
struct PeerControl {
    frames: frame::Reader,
    settings: Option<settings::Reader>,
    started: bool,
    kind: u64,
    value: [u8; 8],
    used: usize,
}

impl Connection {
    pub(crate) fn new(quic: noq::Connection, budget: Budget, role: Role) -> Self {
        let ours: &[(u64, u64)] = match role {
            Role::Server => &settings::SERVER,
            Role::Client => &settings::CLIENT,
        };
        let opening = quic.clone();
        let shared = Arc::new(Shared {
            quic: quic.clone(),
            budget,
            role,
            peer: watch::Sender::new(None),
            state: Mutex::new(State {
                live: 0,
                idle_since: Instant::now(),
                driver: None,
                goaway: None,
                closed: None,
            }),
        });
        Self {
            control: Control {
                opening: Some(Box::pin(async move { opening.open_uni().await })),
                stream: None,
                pending: settings::control_stream(ours),
                written: 0,
            },
            uni: accept_uni(&quic),
            bi: (role == Role::Server).then(|| accept_bi(&quic)),
            streams: Vec::new(),
            critical: [false; 3],
            floor: 3,
            next_request: 0,
            drain: None,
            timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
            closed: false,
            shared,
        }
    }

    /// Server: sends GOAWAY, refuses later requests, and closes once the others end or 5 s pass.
    pub(crate) fn shutdown(&mut self) {
        if self.drain.is_none() && !self.closed {
            frame::put_header(
                frame::GOAWAY,
                varint::len(self.next_request) as u64,
                &mut self.control.pending,
            );
            varint::put(self.next_request, &mut self.control.pending);
            self.drain = Some(Instant::now() + DRAIN);
            if let Some(driver) = self.shared.state().driver.take() {
                driver.wake();
            }
        }
    }

    /// Drives the connection and yields the server's next request stream; `None` once it closed
    /// gracefully, by either side.
    pub(crate) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<RequestStream>, Error>> {
        if self.closed {
            return Poll::Ready(Ok(None));
        }
        let result = ready!(self.poll_inner(cx));
        if matches!(result, Ok(Some(_))) {
            return Poll::Ready(result);
        }
        self.closed = true;
        Poll::Ready(match result {
            Err(Error::Transport(noq::ConnectionError::LocallyClosed)) => match self.shared.state().closed {
                Some(Code::H3_NO_ERROR) | None => Ok(None),
                Some(code) => Err(Error::Connection {
                    local: true,
                    code,
                    reason: Bytes::new(),
                }),
            },
            Err(error) if error.graceful() => Ok(None),
            result => result,
        })
    }

    fn poll_inner(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<RequestStream>, Error>> {
        {
            let mut state = self.shared.state();
            if !state.driver.as_ref().is_some_and(|driver| driver.will_wake(cx.waker())) {
                state.driver = Some(cx.waker().clone());
            }
        }
        self.poll_control(cx)?;
        while let Poll::Ready(stream) = self.uni.as_mut().poll(cx) {
            self.uni = accept_uni(&self.shared.quic);
            self.admit_uni(stream?);
        }
        if let Err(code) = self.poll_streams(cx) {
            return Poll::Ready(Err(self.shared.close(code)));
        }
        while let Some(bi) = &mut self.bi {
            let Poll::Ready(streams) = bi.as_mut().poll(cx) else {
                break;
            };
            *bi = accept_bi(&self.shared.quic);
            let (send, recv) = streams?;
            if let Some(request) = self.admit_request(send, recv) {
                return Poll::Ready(Ok(Some(request)));
            }
        }
        self.poll_timers(cx)
    }

    fn poll_control(&mut self, cx: &mut Context<'_>) -> Result<(), Error> {
        let control = &mut self.control;
        if let Some(opening) = &mut control.opening {
            let Poll::Ready(stream) = opening.as_mut().poll(cx) else {
                return Ok(());
            };
            control.stream = Some(stream?);
            control.opening = None;
        }
        let stream = control.stream.as_mut().expect("opened control stream");
        while control.written < control.pending.len() {
            let Poll::Ready(written) = pin!(stream.write(&control.pending[control.written..])).poll(cx) else {
                return Ok(());
            };
            control.written += written?;
        }
        control.pending.clear();
        control.written = 0;
        Ok(())
    }

    /// A budget refusal stops the new stream, never a critical one within the floor.
    fn admit_uni(&mut self, mut stream: noq::RecvStream) {
        let charge = match self.floor.checked_sub(1) {
            Some(floor) => {
                self.floor = floor;
                None
            }
            None => match Charge::new(&self.shared.budget, size_of::<Uni>()) {
                Some(charge) => Some(charge),
                None => {
                    let _ = stream.stop(Code::H3_REQUEST_REJECTED.into());
                    return;
                }
            },
        };
        let kind = Kind::Unknown {
            header: frame::StreamType::default(),
            deadline: Instant::now() + TYPE_TIMEOUT,
        };
        self.streams.push(Uni {
            stream,
            input: Bytes::new(),
            kind,
            _charge: charge,
        });
    }

    /// Refused past GOAWAY or over the budget: the new stream gets H3_REQUEST_REJECTED.
    fn admit_request(&mut self, mut send: noq::SendStream, mut recv: noq::RecvStream) -> Option<RequestStream> {
        let id: u64 = recv.id().into();
        let late = self.drain.is_some() && id >= self.next_request;
        self.next_request = self.next_request.max(id + 4);
        match stream::charges(&self.shared.budget) {
            Some(charges) if !late => Some(RequestStream::new(
                &self.shared,
                send,
                recv,
                self.shared.role.field_limit(),
                charges,
            )),
            _ => {
                let _ = send.reset(Code::H3_REQUEST_REJECTED.into());
                let _ = recv.stop(Code::H3_REQUEST_REJECTED.into());
                None
            }
        }
    }

    fn poll_streams(&mut self, cx: &mut Context<'_>) -> Result<(), Code> {
        let mut index = 0;
        while index < self.streams.len() {
            if self.poll_uni(index, cx)? {
                index += 1;
            } else {
                self.streams.swap_remove(index);
            }
        }
        Ok(())
    }

    /// Reads one peer stream as far as it goes; `false` once the stream is done with.
    fn poll_uni(&mut self, index: usize, cx: &mut Context<'_>) -> Result<bool, Code> {
        let (shared, critical) = (&self.shared, &mut self.critical);
        let uni = &mut self.streams[index];
        loop {
            match &mut uni.kind {
                Kind::Unknown { header, .. } => {
                    if let Some((kind, _)) = header.read(&mut uni.input) {
                        let (slot, next) = match kind {
                            frame::CONTROL_STREAM => (0, Kind::Control(Box::default())),
                            frame::ENCODER_STREAM => (1, Kind::Encoder),
                            frame::DECODER_STREAM => (2, Kind::Decoder { continuing: false }),
                            frame::PUSH_STREAM if shared.role == Role::Client => return Err(Code::H3_ID_ERROR),
                            frame::PUSH_STREAM => return Err(Code::H3_STREAM_CREATION_ERROR),
                            _ => {
                                let _ = uni.stream.stop(Code::H3_STREAM_CREATION_ERROR.into());
                                return Ok(false);
                            }
                        };
                        if std::mem::replace(&mut critical[slot], true) {
                            return Err(Code::H3_STREAM_CREATION_ERROR);
                        }
                        uni.kind = next;
                        continue;
                    }
                }
                Kind::Control(control) => control.read(shared, &mut uni.input)?,
                Kind::Encoder => {
                    // Only Set Dynamic Table Capacity 0 fits the capacity our SETTINGS allow.
                    if uni.input.iter().any(|&byte| byte != 0x20) {
                        return Err(Code::QPACK_ENCODER_STREAM_ERROR);
                    }
                    uni.input.clear();
                }
                Kind::Decoder { continuing } => {
                    // Only Stream Cancellation fits an encoder that never inserts.
                    for &byte in uni.input.iter() {
                        if *continuing {
                            *continuing = byte & 0x80 != 0;
                        } else if byte & 0xc0 == 0x40 {
                            *continuing = byte & 0x3f == 0x3f;
                        } else {
                            return Err(Code::QPACK_DECODER_STREAM_ERROR);
                        }
                    }
                    uni.input.clear();
                }
            }
            match pin!(uni.stream.read_chunk(usize::MAX)).poll(cx) {
                Poll::Pending => return Ok(true),
                Poll::Ready(Ok(Some(chunk))) => uni.input = chunk,
                Poll::Ready(Err(noq::ReadError::ConnectionLost(_))) => return Ok(true),
                Poll::Ready(_) if matches!(uni.kind, Kind::Unknown { .. }) => return Ok(false),
                Poll::Ready(_) => return Err(Code::H3_CLOSED_CRITICAL_STREAM),
            }
        }
    }

    fn poll_timers(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<RequestStream>, Error>> {
        loop {
            let now = Instant::now();
            self.streams.retain_mut(|uni| match uni.kind {
                Kind::Unknown { deadline, .. } if deadline <= now => {
                    let _ = uni.stream.stop(Code::H3_STREAM_CREATION_ERROR.into());
                    false
                }
                _ => true,
            });
            let (live, idle_since) = {
                let state = self.shared.state();
                (state.live, state.idle_since)
            };
            let idle = (self.shared.role == Role::Server && live == 0).then_some(idle_since + IDLE);
            let finished = self.drain.is_some_and(|drain| live == 0 || drain <= now);
            if finished || idle.is_some_and(|idle| idle <= now) {
                self.shared.close(Code::H3_NO_ERROR);
                return Poll::Ready(Ok(None));
            }
            let types = self.streams.iter().filter_map(|uni| match uni.kind {
                Kind::Unknown { deadline, .. } => Some(deadline),
                _ => None,
            });
            let Some(deadline) = types.chain(idle).chain(self.drain).min() else {
                return Poll::Pending;
            };
            if self.timer.deadline() != deadline {
                self.timer.as_mut().reset(deadline);
            }
            ready!(self.timer.as_mut().poll(cx));
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.close(Code::H3_NO_ERROR);
    }
}

impl PeerControl {
    fn read(&mut self, shared: &Shared, input: &mut Bytes) -> Result<(), Code> {
        while let Some(piece) = self.frames.next(input) {
            match piece {
                Piece::Header { kind, length } if !self.started => {
                    if kind != frame::SETTINGS {
                        return Err(Code::H3_MISSING_SETTINGS);
                    }
                    self.started = true;
                    let mut settings = settings::Reader::new(length)?;
                    match settings.read(&mut Bytes::new())? {
                        Some(peer) => {
                            shared.peer.send_replace(Some(peer));
                        }
                        None => self.settings = Some(settings),
                    }
                }
                Piece::Header { kind, length } => {
                    self.kind = kind;
                    self.used = 0;
                    match kind {
                        frame::MAX_PUSH_ID if shared.role == Role::Client => return Err(Code::H3_FRAME_UNEXPECTED),
                        frame::GOAWAY | frame::MAX_PUSH_ID | frame::CANCEL_PUSH if !(1..=8).contains(&length) => {
                            return Err(Code::H3_FRAME_ERROR);
                        }
                        frame::SETTINGS | frame::DATA | frame::HEADERS | frame::PUSH_PROMISE => {
                            return Err(Code::H3_FRAME_UNEXPECTED);
                        }
                        kind if frame::is_http2(kind) => return Err(Code::H3_FRAME_UNEXPECTED),
                        _ => {}
                    }
                }
                Piece::Payload(mut payload) => {
                    if let Some(settings) = &mut self.settings {
                        if let Some(peer) = settings.read(&mut payload)? {
                            self.settings = None;
                            shared.peer.send_replace(Some(peer));
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
                            if self.kind == frame::GOAWAY && shared.role == Role::Client {
                                let mut state = shared.state();
                                if !value.is_multiple_of(4) || state.goaway.is_some_and(|previous| value > previous) {
                                    return Err(Code::H3_ID_ERROR);
                                }
                                state.goaway = Some(value);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
