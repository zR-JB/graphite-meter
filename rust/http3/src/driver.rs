//! One driver per connection, both roles: control and peer streams, admission, datagrams, CONNECT, GOAWAY, deadlines.
use crate::{
    budget::{Budget, Charge},
    capsule,
    code::Code,
    control,
    error::Error,
    frame,
    incoming::{Incoming, PeerStream},
    settings::{self, Peer},
    stream::{self, RequestStream},
    varint,
    webtransport::{RecvStream, Registry, Sessions, Unrouted},
};
use bytes::Bytes;
use std::{
    future::{Future, poll_fn},
    pin::{Pin, pin},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{
    sync::{Notify, watch},
    time::{Instant, Sleep},
};

/// A server connection without live requests for this long closes.
const IDLE: Duration = Duration::from_secs(15);
const DRAIN: Duration = Duration::from_secs(5);

/// The layer's fixed per-connection state, for the floor: driver, shared stream state, critical streams, futures.
pub const CONNECTION_BYTES: usize = size_of::<Driver>()
    + size_of::<Shared>()
    + 3 * size_of::<PeerStream>()
    + size_of::<control::Reader>()
    + size_of::<Sleep>()
    + size_of::<noq::Stopped>()
    + 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Server,
    Client,
}

/// What a connection's streams and handles share with its driver.
pub(crate) struct Shared {
    pub(crate) quic: noq::Connection,
    pub(crate) budget: Budget,
    pub(crate) role: Role,
    /// The peer's SETTINGS, once they arrived and agree with its transport parameters.
    pub(crate) peer: watch::Sender<Option<Peer>>,
    /// Request stream halves alive.
    live: AtomicUsize,
    going_away: AtomicBool,
    /// The code this side closed the connection with.
    closed: OnceLock<Code>,
    wake: Notify,
    sessions: Mutex<Registry>,
}

impl Shared {
    /// The largest field section this side accepts, as its SETTINGS say.
    pub(crate) fn field_limit(&self) -> u64 {
        match self.role {
            Role::Server => settings::SERVER_FIELD_SECTION,
            Role::Client => settings::CLIENT_FIELD_SECTION,
        }
    }

    pub(crate) fn peer_field_limit(&self) -> Option<u64> {
        self.peer.borrow().and_then(|peer| peer.max_field_section_size)
    }

    /// Whether the server sent GOAWAY, after which this client starts nothing new.
    pub(crate) fn going_away(&self) -> bool {
        self.going_away.load(Ordering::Relaxed)
    }

    pub(crate) fn hold(&self, halves: usize) {
        self.live.fetch_add(halves, Ordering::Relaxed);
    }

    pub(crate) fn release(&self) {
        if self.live.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.wake();
        }
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn sessions(&self) -> MutexGuard<'_, Registry> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Closes on a violation or with H3_NO_ERROR when done; noq reports any earlier close, the peer's too, as local.
    pub(crate) fn close(&self, code: Code) -> Error {
        if self.quic.close_reason().is_none() {
            let _ = self.closed.set(code);
            self.quic.close(code.into(), b"");
        }
        Error::local(code)
    }

    /// Why the connection ended, for what outlives it; this side's own close keeps its code.
    pub(crate) fn close_error(&self) -> Error {
        match (self.quic.close_reason(), self.closed.get()) {
            (Some(noq::ConnectionError::LocallyClosed), Some(&code)) => Error::local(code),
            (reason, _) => reason.map_or(Error::Refused, Error::from),
        }
    }

    /// Takes what the peer's control stream says.
    pub(crate) fn control(&self, event: control::Event) -> Result<(), Code> {
        match event {
            control::Event::Settings(peer) => {
                peer.check(self.quic.max_datagram_size().is_some())?;
                self.peer.send_replace(Some(peer));
            }
            control::Event::Goaway(_) => self.going_away.store(true, Ordering::Relaxed),
        }
        Ok(())
    }
}

pub(crate) struct Driver {
    pub(crate) shared: Arc<Shared>,
    control: Control,
    incoming: Incoming,
    sessions: Sessions,
    /// Server: the lowest request stream ID not yet accepted, and the one our GOAWAY named.
    next_request: u64,
    goaway: Option<u64>,
    drain: Option<Instant>,
    idle_since: Option<Instant>,
    timer: Pin<Box<Sleep>>,
    ended: bool,
}

impl Driver {
    pub(crate) fn new(quic: noq::Connection, budget: Budget, role: Role) -> Self {
        let ours: &[(u64, u64)] = match role {
            Role::Server => &settings::SERVER,
            Role::Client => &settings::CLIENT,
        };
        let opening = quic.clone();
        let shared = Arc::new(Shared {
            quic,
            budget,
            role,
            peer: watch::Sender::new(None),
            live: AtomicUsize::new(0),
            going_away: AtomicBool::new(false),
            closed: OnceLock::new(),
            wake: Notify::new(),
            sessions: Mutex::default(),
        });
        Self {
            shared,
            control: Control {
                stream: ControlStream::Opening(Box::pin(async move {
                    match opening.open_uni().await {
                        Ok(stream) => stream,
                        Err(_) => std::future::pending().await,
                    }
                })),
                pending: settings::control_stream(ours),
                written: 0,
            },
            incoming: Incoming::new(),
            sessions: Sessions::default(),
            next_request: 0,
            goaway: None,
            drain: None,
            idle_since: None,
            timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
            ended: false,
        }
    }

    /// Server: sends GOAWAY; later requests are refused, and the connection closes once the others end.
    pub(crate) fn goaway(&mut self) {
        if self.goaway.is_none() && !self.ended {
            self.goaway = Some(self.next_request);
            frame::put_header(frame::GOAWAY, varint::len(self.next_request) as u64, &mut self.control.pending);
            varint::put(self.next_request, &mut self.control.pending);
        }
    }

    /// Server: [`Self::goaway`], every session ends with `code` and `reason`, and 5 s bound the rest.
    pub(crate) fn shutdown(&mut self, code: u32, reason: &str) {
        self.goaway();
        if self.drain.is_none() && !self.ended {
            self.drain = Some(Instant::now() + DRAIN);
            self.shared.sessions().shutdown(code, reason);
        }
    }

    /// Drives the connection, yielding the server's next request stream; `None` after a graceful close. Cancel-safe.
    pub(crate) async fn next(&mut self) -> Result<Option<RequestStream>, Error> {
        if self.ended {
            return Ok(None);
        }
        let result = self.run().await;
        if matches!(result, Ok(Some(_))) {
            return result;
        }
        self.end();
        match result {
            Err(Error::Transport(noq::ConnectionError::LocallyClosed)) => match self.shared.close_error() {
                error if error.graceful() => Ok(None),
                error => Err(error),
            },
            Err(error) if error.graceful() => Ok(None),
            result => result,
        }
    }

    async fn run(&mut self) -> Result<Option<RequestStream>, Error> {
        let shared = self.shared.clone();
        let mut datagrams = pin!(route_datagrams(&shared));
        loop {
            let now = Instant::now();
            self.incoming.expire(now);
            shared.sessions().expire(now);
            let close_at = self.close_at(now);
            if close_at.is_some_and(|close_at| close_at <= now) {
                shared.close(Code::H3_NO_ERROR);
                return Ok(None);
            }
            let early = shared.sessions().deadline();
            let deadline = [close_at, self.incoming.deadline(), early, self.sessions.deadline()]
                .into_iter()
                .flatten()
                .min();
            if let Some(deadline) = deadline.filter(|&deadline| deadline != self.timer.deadline()) {
                self.timer.as_mut().reset(deadline);
            }
            tokio::select! {
                biased;
                code = poll_fn(|cx| self.control.poll(cx)) => return Err(shared.close(code)),
                stream = shared.quic.accept_uni() => self.incoming.admit(stream?, &shared.budget),
                read = poll_fn(|cx| self.incoming.poll(cx, &shared)) => match read {
                    Ok((session, stream, first)) => route(&shared, session, stream, first),
                    Err(code) => return Err(shared.close(code)),
                },
                error = &mut datagrams => return Err(error),
                streams = shared.quic.accept_bi(), if shared.role == Role::Server => {
                    if let Some(request) = self.admit(streams?) {
                        return Ok(Some(request));
                    }
                }
                () = poll_fn(|cx| self.sessions.poll(cx, &shared)) => {}
                () = shared.wake.notified() => {}
                () = &mut self.timer, if deadline.is_some() => {}
            }
        }
    }

    /// When this server connection closes with H3_NO_ERROR, if anything bounds it.
    fn close_at(&mut self, now: Instant) -> Option<Instant> {
        if self.shared.role == Role::Client {
            return None;
        }
        if self.shared.live.load(Ordering::Relaxed) > 0 {
            self.idle_since = None;
            return self.drain;
        }
        let idle = *self.idle_since.get_or_insert(now) + IDLE;
        let sessions = self.shared.sessions();
        // A connection that only carried sessions ends with its last one: browsers would hold its slot.
        let done = (self.goaway.is_some() || sessions.only_sessions()).then(|| sessions.linger.unwrap_or(now));
        [self.drain, Some(idle), done].into_iter().flatten().min()
    }

    /// Refused past GOAWAY or over the budget, a new stream gets H3_REQUEST_REJECTED.
    fn admit(&mut self, (mut send, mut recv): (noq::SendStream, noq::RecvStream)) -> Option<RequestStream> {
        let id = u64::from(recv.id());
        self.next_request = self.next_request.max(id + 4);
        match stream::charges(&self.shared.budget) {
            Some(charges) if self.goaway.is_none_or(|goaway| id < goaway) => {
                // A request that ends before the next pass still restarts the idle period.
                self.idle_since = None;
                Some(RequestStream::new(&self.shared, send, recv, charges))
            }
            _ => {
                let _ = send.reset(Code::H3_REQUEST_REJECTED.into());
                let _ = recv.stop(Code::H3_REQUEST_REJECTED.into());
                None
            }
        }
    }

    /// The connection ended: so does every session, also one handed over from now on.
    fn end(&mut self) {
        if !std::mem::replace(&mut self.ended, true) {
            self.sessions.end(&self.shared, &self.shared.close_error());
        }
    }
}

/// Hands a peer's session stream to the registry; one the budget cannot hold is refused as unbuffered.
fn route(shared: &Shared, session: u64, mut stream: noq::RecvStream, first: Bytes) {
    match Charge::new(&shared.budget, size_of::<RecvStream>()) {
        Some(charge) => shared.sessions().stream(session, Unrouted { stream, first, charge }),
        None => drop(stream.stop(Code::WT_BUFFERED_STREAM_REJECTED.into())),
    }
}

/// Routes each datagram to its session; ends only with the connection or a malformed session ID.
async fn route_datagrams(shared: &Shared) -> Error {
    loop {
        match shared.quic.read_datagram().await.map(capsule::datagram) {
            Ok(Ok((session, payload))) => shared.sessions().datagram(session, payload),
            Ok(Err(code)) => return shared.close(code),
            Err(error) => return error.into(),
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.shared.close(Code::H3_NO_ERROR);
        self.end();
    }
}

/// Our control stream: SETTINGS first, GOAWAY on shutdown.
struct Control {
    stream: ControlStream,
    pending: Vec<u8>,
    written: usize,
}

enum ControlStream {
    /// Kept across passes, as noq wakes only a waiting open; it never resolves once the connection closed.
    Opening(Pin<Box<dyn Future<Output = noq::SendStream> + Send>>),
    /// With its STOP_SENDING, which the peer must never send (RFC 9114 §6.2.1).
    Open(noq::SendStream, Pin<Box<noq::Stopped>>),
}

impl Control {
    /// Opens the stream and writes what is queued; `Ready` with the code the connection closes with.
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Code> {
        if let ControlStream::Opening(opening) = &mut self.stream {
            let stream = ready!(opening.as_mut().poll(cx));
            let stopped = Box::pin(stream.stopped());
            self.stream = ControlStream::Open(stream, stopped);
        }
        let ControlStream::Open(stream, stopped) = &mut self.stream else {
            unreachable!("opened above")
        };
        if let Poll::Ready(Ok(Some(_))) = stopped.as_mut().poll(cx) {
            return Poll::Ready(Code::H3_CLOSED_CRITICAL_STREAM);
        }
        while self.written < self.pending.len() {
            match ready!(pin!(stream.write(&self.pending[self.written..])).poll(cx)) {
                Ok(written) => self.written += written,
                Err(noq::WriteError::Stopped(_)) => return Poll::Ready(Code::H3_CLOSED_CRITICAL_STREAM),
                Err(_) => return Poll::Pending,
            }
        }
        self.pending.clear();
        self.written = 0;
        Poll::Pending
    }
}
