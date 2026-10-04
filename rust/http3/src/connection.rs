//! The connection driver for both roles: our control stream, the peer's control and QPACK streams,
//! stream classification, GOAWAY and idle. Streams hold [`Shared`], the state they need from it.
use crate::{
    capsule,
    charge::{Budget, Charge},
    code::Code,
    control,
    error::Error,
    frame, qpack,
    settings::{self, Peer},
    stream::{self, RequestStream},
    varint,
    webtransport::{Connect, PendingReset, RecvStream, Registry},
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
    sync::{mpsc, watch},
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
    /// Sessions hand their CONNECT streams to the driver.
    pub(crate) connects: mpsc::UnboundedSender<Connect>,
    state: Mutex<State>,
}

pub(crate) struct State {
    /// Request stream halves alive; a server connection without any for a while closes.
    live: usize,
    idle_since: Instant,
    driver: Option<Waker>,
    /// The lowest stream ID of the server's GOAWAYs.
    goaway: Option<u64>,
    /// The code this side closed the connection with.
    closed: Option<Code>,
    pub(crate) sessions: Registry,
    resets: Vec<PendingReset>,
}

impl State {
    pub(crate) fn wake(&mut self) {
        if let Some(driver) = self.driver.take() {
            driver.wake();
        }
    }
}

impl Shared {
    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("HTTP/3 state poisoned")
    }

    /// Hands a cancelled stream's unfinished association header to the driver.
    pub(crate) fn defer_reset(&self, mut pending: PendingReset) {
        let Some(charge) = Charge::new(&self.budget, size_of::<PendingReset>()) else {
            return pending.abandon();
        };
        pending._charge = Some(charge);
        let mut state = self.state();
        state.resets.push(pending);
        state.wake();
    }

    pub(crate) fn hold(&self, halves: usize) {
        self.state().live += halves;
    }

    pub(crate) fn release(&self) {
        let mut state = self.state();
        state.live -= 1;
        if state.live == 0 {
            state.idle_since = Instant::now();
            state.wake();
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
        Error::local(code)
    }

    /// Why the connection ended, for what outlives it; this side's own close keeps its code.
    pub(crate) fn close_error(&self) -> Error {
        match (self.quic.close_reason(), self.state().closed) {
            (Some(noq::ConnectionError::LocallyClosed), Some(code)) => Error::local(code),
            (reason, _) => reason.map_or(Error::Refused, Error::from),
        }
    }
}

type Pending<T> = Pin<Box<dyn Future<Output = Result<T, noq::ConnectionError>> + Send>>;

/// Routes each datagram to its session; ends only with the connection or a malformed ID.
async fn datagrams(shared: Arc<Shared>) -> Result<(), Error> {
    loop {
        let datagram = shared.quic.read_datagram().await?;
        let (session, payload) = capsule::datagram(datagram).map_err(|code| shared.close(code))?;
        shared.state().sessions.datagram(session, payload);
    }
}

/// The layer's fixed state per connection, for the application's connection floor: the driver,
/// what its streams share, the peer's control and QPACK streams, and four boxed stream futures.
pub const CONNECTION_BYTES: usize =
    size_of::<Connection>() + size_of::<Shared>() + 3 * size_of::<Uni>() + size_of::<control::Reader>() + 4 * 256;

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
    datagrams: Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>,
    streams: Vec<Uni>,
    resets: Vec<PendingReset>,
    connects: mpsc::UnboundedReceiver<Connect>,
    /// The session's CONNECT stream, and earlier ones still finishing their close.
    sessions: Vec<Connect>,
    /// Whether the peer opened its control, QPACK encoder and QPACK decoder streams.
    critical: [bool; 3],
    /// Streams still covered by [`CONNECTION_BYTES`] instead of a charge.
    floor: u8,
    /// Server: the lowest request stream ID not yet accepted, GOAWAY, and the shutdown's deadline.
    next_request: u64,
    goaway_sent: bool,
    drain: Option<Instant>,
    timer: Pin<Box<Sleep>>,
    closed: bool,
}

/// Our control stream: SETTINGS first, GOAWAY on shutdown.
struct Control {
    opening: Option<Pending<noq::SendStream>>,
    /// The stream, and its STOP_SENDING, which the peer must never send (RFC 9114 §6.2.1).
    stream: Option<(noq::SendStream, Pin<Box<noq::Stopped>>)>,
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
    Control(Box<control::Reader>),
    Encoder,
    Decoder(qpack::DecoderStream),
    /// Classified; the driver hands it to the session registry.
    Session(u64),
}

impl Connection {
    pub(crate) fn new(quic: noq::Connection, budget: Budget, role: Role) -> Self {
        let ours: &[(u64, u64)] = match role {
            Role::Server => &settings::SERVER,
            Role::Client => &settings::CLIENT,
        };
        let opening = quic.clone();
        let (connects, connecting) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            quic: quic.clone(),
            budget,
            role,
            peer: watch::Sender::new(None),
            connects,
            state: Mutex::new(State {
                live: 0,
                idle_since: Instant::now(),
                driver: None,
                goaway: None,
                closed: None,
                sessions: Registry::default(),
                resets: Vec::new(),
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
            datagrams: Box::pin(datagrams(shared.clone())),
            streams: Vec::new(),
            resets: Vec::new(),
            connects: connecting,
            sessions: Vec::new(),
            critical: [false; 3],
            floor: 3,
            next_request: 0,
            goaway_sent: false,
            drain: None,
            timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
            closed: false,
            shared,
        }
    }

    /// Server: sends GOAWAY; later requests are refused, and the connection closes once the others end.
    pub(crate) fn goaway(&mut self) {
        if !self.goaway_sent && !self.closed {
            self.goaway_sent = true;
            frame::put_header(frame::GOAWAY, varint::len(self.next_request) as u64, &mut self.control.pending);
            varint::put(self.next_request, &mut self.control.pending);
            self.shared.state().wake();
        }
    }

    /// Server: [`Self::goaway`], every session ends with `code` and `reason`, and 5 s bound the rest.
    pub(crate) fn shutdown(&mut self, code: u32, reason: &str) {
        self.goaway();
        if self.drain.is_none() && !self.closed {
            self.drain = Some(Instant::now() + DRAIN);
            let mut state = self.shared.state();
            state.sessions.shutdown(code, reason);
            state.wake();
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
        self.end_sessions();
        Poll::Ready(match result {
            Err(Error::Transport(noq::ConnectionError::LocallyClosed)) => match self.shared.state().closed {
                Some(Code::H3_NO_ERROR) | None => Ok(None),
                Some(code) => Err(Error::local(code)),
            },
            Err(error) if error.graceful() => Ok(None),
            result => result,
        })
    }

    /// Every session ends with the connection: its streams and datagrams end, and `closed` says why.
    fn end_sessions(&mut self) {
        self.shared.state().sessions.end();
        // A session awaiting SETTINGS that never came learns the connection ended.
        self.shared.peer.send_modify(|_| {});
        // A session handed over from now on finds no driver, and its CONNECT stream goes.
        self.connects.close();
        while let Ok(connect) = self.connects.try_recv() {
            self.sessions.push(connect);
        }
        let error = self.shared.close_error();
        for connect in self.sessions.drain(..) {
            connect.end(Err(error.clone()));
        }
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
        if let Poll::Ready(Err(error)) = self.datagrams.as_mut().poll(cx) {
            return Poll::Ready(Err(error));
        }
        let now = Instant::now();
        self.resets.append(&mut self.shared.state().resets);
        self.resets.retain_mut(|pending| !pending.poll(cx, now));
        while let Poll::Ready(Some(connect)) = self.connects.poll_recv(cx) {
            self.sessions.push(connect);
        }
        self.sessions.retain_mut(|connect| !connect.poll(cx, now, &self.shared));
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
        self.poll_timers(cx, now)
    }

    /// The peer stopping it ends the connection with H3_CLOSED_CRITICAL_STREAM, noticed without a write.
    fn poll_control(&mut self, cx: &mut Context<'_>) -> Result<(), Error> {
        let control = &mut self.control;
        if let Some(opening) = &mut control.opening {
            let Poll::Ready(stream) = opening.as_mut().poll(cx) else {
                return Ok(());
            };
            let stream = stream?;
            let stopped = Box::pin(stream.stopped());
            control.stream = Some((stream, stopped));
            control.opening = None;
        }
        let (stream, stopped) = control.stream.as_mut().expect("opened control stream");
        if let Poll::Ready(Ok(Some(_))) = stopped.as_mut().poll(cx) {
            return Err(self.shared.close(Code::H3_CLOSED_CRITICAL_STREAM));
        }
        while control.written < control.pending.len() {
            let Poll::Ready(written) = pin!(stream.write(&control.pending[control.written..])).poll(cx) else {
                return Ok(());
            };
            control.written += match written {
                Ok(written) => written,
                Err(noq::WriteError::Stopped(_)) => return Err(self.shared.close(Code::H3_CLOSED_CRITICAL_STREAM)),
                Err(error) => return Err(error.into()),
            };
        }
        control.pending.clear();
        control.written = 0;
        Ok(())
    }

    /// A budget refusal stops the new stream, never a critical one within the floor.
    fn admit_uni(&mut self, mut stream: noq::RecvStream) {
        let charge = if let Some(floor) = self.floor.checked_sub(1) {
            self.floor = floor;
            None
        } else if let Some(charge) = Charge::new(&self.shared.budget, size_of::<Uni>()) {
            Some(charge)
        } else {
            let _ = stream.stop(Code::H3_REQUEST_REJECTED.into());
            return;
        };
        let kind = Kind::Unknown {
            header: frame::StreamType::default(),
            deadline: Instant::now() + TYPE_TIMEOUT,
        };
        self.streams
            .push(Uni { stream, input: Bytes::new(), kind, _charge: charge });
    }

    /// Refused past GOAWAY or over the budget: the new stream gets H3_REQUEST_REJECTED.
    fn admit_request(&mut self, mut send: noq::SendStream, mut recv: noq::RecvStream) -> Option<RequestStream> {
        let id: u64 = recv.id().into();
        let late = self.goaway_sent && id >= self.next_request;
        self.next_request = self.next_request.max(id + 4);
        match stream::charges(&self.shared.budget) {
            Some(charges) if !late => Some(RequestStream::new(&self.shared, send, recv, charges)),
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
                continue;
            }
            let uni = self.streams.swap_remove(index);
            if let Kind::Session(session) = uni.kind {
                match Charge::new(&self.shared.budget, size_of::<RecvStream>()) {
                    Some(charge) => {
                        let stream = RecvStream::new(uni.stream, uni.input, charge);
                        self.shared.state().sessions.stream(session, stream);
                    }
                    None => {
                        let mut stream = uni.stream;
                        let _ = stream.stop(Code::WT_BUFFERED_STREAM_REJECTED.into());
                    }
                }
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
                    if let Some((kind, session)) = header.read(&mut uni.input) {
                        if let Some(session) = session {
                            // Only a client-initiated bidirectional stream can carry a session.
                            if !session.is_multiple_of(4) {
                                return Err(Code::H3_ID_ERROR);
                            }
                            uni.kind = Kind::Session(session);
                            return Ok(false);
                        }
                        let (slot, next) = match kind {
                            frame::CONTROL_STREAM => (0, Kind::Control(Box::default())),
                            frame::ENCODER_STREAM => (1, Kind::Encoder),
                            frame::DECODER_STREAM => (2, Kind::Decoder(qpack::DecoderStream::default())),
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
                Kind::Control(control) => {
                    control.read(shared.role == Role::Client, &mut uni.input, |event| match event {
                        control::Event::Settings(peer) => settled(shared, peer),
                        control::Event::Goaway(id) => {
                            shared.state().goaway = Some(id);
                            Ok(())
                        }
                    })?
                }
                Kind::Encoder => {
                    qpack::encoder_stream(&uni.input)?;
                    uni.input.clear();
                }
                Kind::Decoder(decoder) => {
                    decoder.read(&uni.input)?;
                    uni.input.clear();
                }
                Kind::Session(_) => return Ok(false),
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

    /// Everything due by `now` was handled, so the timer waits for a later deadline.
    fn poll_timers(&mut self, cx: &mut Context<'_>, now: Instant) -> Poll<Result<Option<RequestStream>, Error>> {
        self.streams.retain_mut(|uni| match uni.kind {
            Kind::Unknown { deadline, .. } if deadline <= now => {
                let _ = uni.stream.stop(Code::H3_STREAM_CREATION_ERROR.into());
                false
            }
            _ => true,
        });
        let (live, idle_since, sessions_only, reordering, linger) = {
            let mut state = self.shared.state();
            let sessions = &mut state.sessions;
            let sessions_only = sessions.carried && !sessions.served;
            let reordering = sessions.expire(now);
            let linger = sessions.linger.filter(|linger| *linger > now);
            (state.live, state.idle_since, sessions_only, reordering, linger)
        };
        let server = self.shared.role == Role::Server;
        let idle = (server && live == 0).then_some(idle_since + IDLE);
        // A connection that only carried sessions ends with its last one: browsers would hold its slot.
        let done = live == 0 && linger.is_none() && (self.goaway_sent || server && sessions_only);
        if done || self.drain.is_some_and(|drain| drain <= now) || idle.is_some_and(|idle| idle <= now) {
            self.shared.close(Code::H3_NO_ERROR);
            return Poll::Ready(Ok(None));
        }
        let types = self.streams.iter().filter_map(|uni| match uni.kind {
            Kind::Unknown { deadline, .. } => Some(deadline),
            _ => None,
        });
        let resets = self.resets.iter().map(|pending| pending.deadline);
        let closing = self.sessions.iter().filter_map(|connect| connect.deadline);
        let Some(deadline) = types
            .chain(resets)
            .chain(closing)
            .chain(reordering)
            .chain(linger)
            .chain(idle)
            .chain(self.drain)
            .min()
        else {
            return Poll::Pending;
        };
        if self.timer.deadline() != deadline {
            self.timer.as_mut().reset(deadline);
        }
        if self.timer.as_mut().poll(cx).is_ready() {
            // It passed while this pass ran: run another with a later clock.
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.shared.close(Code::H3_NO_ERROR);
        self.end_sessions();
    }
}

/// Publishes the peer's SETTINGS once they agree with its transport parameters.
fn settled(shared: &Shared, peer: Peer) -> Result<(), Code> {
    peer.check(shared.quic.max_datagram_size().is_some())?;
    shared.peer.send_replace(Some(peer));
    Ok(())
}
