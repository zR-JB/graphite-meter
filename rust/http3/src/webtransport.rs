//! WebTransport over HTTP/3 in either dialect, one session per connection: streams, datagrams, resets, close sequence.

use crate::{
    budget::Charge,
    capsule,
    client::SendRequest,
    code::{Code, WtCode},
    driver::Shared,
    error::Error,
    frame::{self, Header},
    settings::Dialect,
    stream::{RequestStream, poll_write_header},
};
use bytes::Bytes;
use std::{
    collections::VecDeque,
    future::{Future, poll_fn},
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc, watch},
    time::Instant,
};

/// How long a server waits for its client's SETTINGS, as webtransport-go's does.
const SETTINGS_WAIT: Duration = Duration::from_secs(5);
/// How long the refusal of a CONNECT without a dialect may take to write.
const REFUSAL_TIMEOUT: Duration = Duration::from_secs(10);
/// State a session keeps apart from its CONNECT stream: queue slots, the capsule reader and both sides' close reasons.
const SESSION_BYTES: usize = size_of::<Session>()
    + size_of::<Connect>()
    + STREAM_QUEUE * size_of::<RecvStream>()
    + DATAGRAM_QUEUE * size_of::<Bytes>()
    + 4
    + 2 * capsule::MAX_REASON;

/// Where a session is, which its streams watch: the drafts end them with it.
#[derive(Clone, Debug)]
pub(crate) enum Phase {
    /// The server's 200 head is not yet in QUIC's stream buffer.
    Opening,
    Open,
    /// With a close's code and reason, or the error its connection ended with.
    Ended(Result<(u32, String), Error>),
}

impl Phase {
    pub(crate) fn is_ended(&self) -> bool {
        matches!(self, Self::Ended(_))
    }
}

/// `work`'s outcome, refused once the session ended, even mid-wait as in webtransport-go; ready work watches nothing.
async fn unless_ended<T>(session: &watch::Receiver<Phase>, work: impl Future<Output = T>) -> Result<T, Error> {
    let gone = session.borrow().is_ended();
    tokio::select! {
        biased;
        done = work, if !gone => Ok(done),
        _ = async { session.clone().wait_for(Phase::is_ended).await.map(drop) } => Err(Error::Refused),
    }
}

/// A WebTransport session; `&self` methods let lanes share it, and dropping it unclosed finishes it with code 0.
pub struct Session {
    id: u64,
    shared: Arc<Shared>,
    datagram_prefix: ([u8; 8], usize),
    streams: Mutex<mpsc::Receiver<RecvStream>>,
    datagrams: Mutex<mpsc::Receiver<Bytes>>,
    phase: watch::Receiver<Phase>,
}

/// A payload encoded for one session, sent repeatedly without copying it.
pub struct PreparedDatagram<'a> {
    session: &'a Session,
    bytes: Bytes,
}

impl PreparedDatagram<'_> {
    /// Sends the payload once the queue has room, refused if the session ends while waiting.
    pub async fn send_wait(&mut self) -> Result<(), Error> {
        let sent = self.session.shared.quic.send_datagram_wait(self.bytes.clone());
        Ok(unless_ended(&self.session.phase, sent).await??)
    }
}

impl Session {
    /// Server: accepts a CONNECT; no WebTransport and datagrams in 5 s gets 400, a second session H3_REQUEST_REJECTED.
    pub async fn accept(mut stream: RequestStream, headers: http::HeaderMap) -> Result<Self, Error> {
        let shared = stream.shared().clone();
        let mut response = http::Response::new(());
        *response.headers_mut() = headers;
        let Some(dialect) = dialect(&shared, SETTINGS_WAIT).await.ok().flatten() else {
            *response.status_mut() = http::StatusCode::BAD_REQUEST;
            let answer = async {
                stream.send.send_response(response).await?;
                stream.send.finish().await
            };
            match tokio::time::timeout(REFUSAL_TIMEOUT, answer).await {
                Ok(answered) => answered?,
                Err(_) => stream.send.reset(Code::H3_REQUEST_CANCELLED),
            }
            return Err(Error::Refused);
        };
        if dialect == Dialect::Draft02 {
            // Draft 02 requires it; Go omits it.
            let draft = http::HeaderValue::from_static("draft02");
            response.headers_mut().insert("sec-webtransport-http3-draft", draft);
        }
        // The driver writes it before any capsule.
        stream.send.queue_response(response)?;
        Self::register(stream, Code::H3_REQUEST_REJECTED, Phase::Opening)
    }

    /// Client: opens a session in the server's dialect once SETTINGS arrive; refusals return their response.
    pub async fn connect(
        requests: &SendRequest,
        request: http::Request<()>,
    ) -> Result<Result<(Self, http::Response<()>), http::Response<()>>, Error> {
        let shared = &requests.0;
        let dialect = dialect(shared, Duration::MAX).await?;
        let connect_protocol = shared.peer.borrow().is_some_and(|peer| peer.connect_protocol);
        if shared.going_away() {
            return Err(Error::GoingAway);
        }
        let (mut parts, ()) = request.into_parts();
        parts.method = http::Method::CONNECT;
        let protocol = match dialect.filter(|_| connect_protocol).ok_or(Error::NoWebTransport)? {
            Dialect::Draft02 => {
                parts
                    .headers
                    .insert("sec-webtransport-http3-draft02", http::HeaderValue::from_static("1"));
                "webtransport"
            }
            Dialect::Draft15 => "webtransport-h3",
        };
        let mut stream = requests.open(parts, Some(protocol)).await?;
        let response = stream.recv.response().await?;
        if !response.status().is_success() {
            return Ok(Err(response));
        }
        Self::register(stream, Code::H3_REQUEST_CANCELLED, Phase::Open).map(|session| Ok((session, response)))
    }

    /// Makes the stream the connection's one session and hands it to the driver; a second gets `refusal`.
    fn register(mut stream: RequestStream, refusal: Code, opened: Phase) -> Result<Self, Error> {
        let (shared, id) = (stream.shared().clone(), stream.id());
        let mut sessions = shared.sessions();
        let Some(charge) = Charge::new(&shared.budget, SESSION_BYTES).filter(|_| sessions.vacant()) else {
            drop(sessions);
            stream.abort(refusal);
            return Err(Error::Refused);
        };
        let (phase, watching) = watch::channel(opened);
        let (streams, datagrams) = sessions.register(Connect::new(id, stream, phase, charge));
        drop(sessions);
        shared.wake();
        Ok(Self {
            id,
            datagram_prefix: capsule::datagram_prefix(id),
            streams: Mutex::new(streams),
            datagrams: Mutex::new(datagrams),
            phase: watching,
            shared,
        })
    }

    /// The next stream the peer opened in this session; `None` once the session ended.
    pub async fn accept_uni(&self) -> Option<RecvStream> {
        self.streams.lock().await.recv().await
    }

    /// The next datagram in this session; `None` once the session ended.
    pub async fn read_datagram(&self) -> Option<Bytes> {
        self.datagrams.lock().await.recv().await
    }

    /// Waits for the 200 head to buffer, so unread stream data cannot take its credit; refused once the session ended.
    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        let mut phase = self.phase.clone();
        let opened = phase.wait_for(|phase| !matches!(phase, Phase::Opening)).await;
        if opened.map_or(true, |phase| phase.is_ended()) {
            return Err(Error::Refused);
        }
        SendStream::open(&self.shared, self.id, self.phase.clone()).await
    }

    /// A datagram's bytes, refused once the session ended: the drafts send none after it.
    fn datagram(&self, payload: &[u8]) -> Result<Bytes, Error> {
        if self.phase.borrow().is_ended() {
            return Err(Error::Refused);
        }
        let (prefix, length) = &self.datagram_prefix;
        Ok([&prefix[..*length], payload].concat().into())
    }

    /// Sends a datagram, displacing the oldest unsent ones if the queue is full.
    pub fn send_datagram(&self, payload: &[u8]) -> Result<(), Error> {
        Ok(self.shared.quic.send_datagram(self.datagram(payload)?)?)
    }

    /// Encodes a datagram for repeated sends in this session; each send obeys the queue's limits.
    pub fn prepare_datagram(&self, payload: &[u8]) -> Result<PreparedDatagram<'_>, Error> {
        Ok(PreparedDatagram { session: self, bytes: self.datagram(payload)? })
    }

    /// Sends a datagram once the queue has room.
    pub async fn send_datagram_wait(&self, payload: &[u8]) -> Result<(), Error> {
        self.prepare_datagram(payload)?.send_wait().await
    }

    /// Resolves at session end: the peer's CLOSE, 0 for a bare FIN, this side's earlier close, or the connection error.
    pub async fn closed(&self) -> Result<(u32, String), Error> {
        let mut phase = self.phase.clone();
        match phase.wait_for(Phase::is_ended).await.as_deref() {
            Ok(Phase::Ended(ending)) => ending.clone(),
            _ => Err(self.shared.close_error()),
        }
    }

    /// Ends the session with `code` and `reason`; returns after CLOSE, FIN, the peer's FIN within 1 s and STOP_SENDING.
    pub async fn close(&self, code: u32, reason: &str) {
        self.request_close(code, reason);
        let mut phase = self.phase.clone();
        while phase.changed().await.is_ok() {}
    }

    fn request_close(&self, code: u32, reason: &str) {
        self.shared.sessions().request_close(self.id, code, reason);
        self.shared.wake();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.request_close(0, "");
        // Streams the application never took belong to a session that is gone.
        let streams = self.streams.get_mut();
        streams.close();
        while let Ok(mut stream) = streams.try_recv() {
            stream.stop(Code::WT_SESSION_GONE);
        }
    }
}

/// The WebTransport rules applied to the peer's SETTINGS, awaited for `wait`, or the connection's earlier error.
async fn dialect(shared: &Shared, wait: Duration) -> Result<Option<Dialect>, Error> {
    let mut peer = shared.peer.subscribe();
    let settled = async {
        tokio::select! {
            peer = peer.wait_for(Option::is_some) => Ok(peer.ok().and_then(|peer| *peer)),
            _ = shared.quic.closed() => Err(shared.close_error()),
        }
    };
    let peer = tokio::time::timeout(wait, settled)
        .await
        .map_err(|_| Error::TimedOut)??;
    Ok(peer.and_then(|peer| peer.webtransport(shared.quic.max_datagram_size().is_some())))
}

/// How long a closing session waits for the peer's FIN, and a sessions-only connection for the CLOSE.
const CLOSE_WAIT: Duration = Duration::from_secs(1);
/// Control data a peer may send on its CONNECT stream.
const MAX_CONNECT_BYTES: u64 = 1024 * 1024;
const MAX_CONNECT_CHUNKS: u64 = 1024;
const MAX_DEFERRED_RESETS: usize = 64;

#[derive(Default)]
pub(crate) struct Sessions {
    connects: Vec<Connect>,
    resets: Vec<PendingReset>,
}

impl Sessions {
    /// Runs every CONNECT stream and pending reset; `Ready` once one is done with.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, shared: &Shared) -> Poll<()> {
        let (connects, resets) = shared.sessions().take();
        self.connects.extend(connects);
        for reset in resets {
            self.defer(reset, shared);
        }
        let now = Instant::now();
        let before = self.connects.len() + self.resets.len();
        self.connects.retain_mut(|connect| !connect.poll(cx, now, shared));
        self.resets.retain_mut(|reset| !reset.poll(cx, now));
        if self.connects.len() + self.resets.len() < before {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Queues a reset within the bound and the budget; past either it resets without its header.
    fn defer(&mut self, mut reset: PendingReset, shared: &Shared) {
        reset.charge = Charge::new(&shared.budget, size_of::<PendingReset>());
        if self.resets.len() < MAX_DEFERRED_RESETS && reset.charge.is_some() {
            self.resets.push(reset);
        } else {
            reset.abandon();
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        let closing = self.connects.iter().filter_map(|connect| connect.deadline);
        closing.chain(self.resets.iter().map(|reset| reset.deadline)).min()
    }

    /// The connection ended with `error`: every session ends with it, and no session follows.
    pub(crate) fn end(&mut self, shared: &Shared, error: &Error) {
        let (connects, resets) = shared.sessions().end();
        for connect in self.connects.drain(..).chain(connects) {
            connect.end(Err(error.clone()));
        }
        let abandoned = self.resets.drain(..).chain(resets);
        abandoned.for_each(|mut reset| reset.abandon());
    }
}

/// A session's CONNECT stream: the driver reads the peer's capsules and runs the close.
pub(crate) struct Connect {
    pub(super) id: u64,
    stream: RequestStream,
    capsules: capsule::Reader,
    input: Bytes,
    /// Control data the peer sent: bytes and chunks.
    received: (u64, u64),
    pub(super) phase: watch::Sender<Phase>,
    /// Once the session ended: when the wait for the peer's FIN gives up.
    deadline: Option<Instant>,
    peer_finished: bool,
    /// The peer's CLOSE arrived, so only its FIN may follow.
    peer_closed: bool,
    /// This side ended the session, so a sessions-only connection lingers for its CLOSE.
    closed_here: bool,
    _charge: Charge,
}

impl Connect {
    pub(super) fn new(id: u64, stream: RequestStream, phase: watch::Sender<Phase>, charge: Charge) -> Self {
        Self {
            id,
            stream,
            capsules: capsule::Reader::default(),
            input: Bytes::new(),
            received: (0, 0),
            phase,
            deadline: None,
            peer_finished: false,
            peer_closed: false,
            closed_here: false,
            _charge: charge,
        }
    }

    /// Ends the session: CLOSE unless peer-ended, FIN, the peer's FIN within 1 s, then STOP_SENDING; `true` once done.
    fn poll(&mut self, cx: &mut Context<'_>, now: Instant, shared: &Shared) -> bool {
        let failed = match self.poll_read(cx) {
            Ok(()) if self.deadline.is_none() => match self.stream.send.poll_ready(cx) {
                Poll::Ready(Ok(())) => {
                    self.phase.send_if_modified(|phase| {
                        matches!(phase, Phase::Opening) && {
                            *phase = Phase::Open;
                            true
                        }
                    });
                    None
                }
                Poll::Ready(Err(error)) => Some(error),
                Poll::Pending => None,
            },
            read => read.err(),
        };
        if let Some(error) = failed {
            self.end(Err(error));
            return self.done(now, shared);
        }
        let Some(deadline) = self.deadline.or_else(|| self.close(now, shared)) else {
            return false;
        };
        let finished = self.stream.send.poll_finish(cx).is_ready();
        if !(finished && self.peer_finished) && now < deadline {
            return false;
        }
        // Browsers drop the code of a CLOSE whose STOP_SENDING arrives first.
        self.stream.send.reset(Code::WT_SESSION_GONE);
        self.stream.recv.stop(Code::WT_SESSION_GONE);
        self.done(now, shared)
    }

    /// Starts the close once the session ended or this side asked; returns when the wait for FIN ends.
    fn close(&mut self, now: Instant, shared: &Shared) -> Option<Instant> {
        if !self.phase.borrow().is_ended() {
            let (code, reason) = shared.sessions().requested_close(self.id)?;
            // An unwritten 200 head goes first.
            self.stream
                .send
                .queue_after(frame::DATA, &capsule::close(code, &reason));
            self.end(Ok((code, reason)));
            self.closed_here = true;
        }
        shared.sessions().unregister(self.id);
        Some(*self.deadline.insert(now + CLOSE_WAIT))
    }

    fn done(&self, now: Instant, shared: &Shared) -> bool {
        let mut sessions = shared.sessions();
        sessions.unregister(self.id);
        if self.closed_here {
            sessions.linger = Some(now + CLOSE_WAIT);
        }
        true
    }

    /// Reads capsules to the peer's FIN; its CLOSE or a bare FIN ends the session; data past CLOSE is H3_MESSAGE_ERROR.
    fn poll_read(&mut self, cx: &mut Context<'_>) -> Result<(), Error> {
        while !self.peer_finished {
            if self.peer_closed && !self.input.is_empty() {
                return Err(self.stream.abort(Code::H3_MESSAGE_ERROR));
            }
            match self.capsules.read(&mut self.input) {
                Err(code) => return Err(self.stream.abort(code)),
                Ok(Some(close)) => {
                    self.peer_closed = true;
                    self.end(Ok(close));
                }
                Ok(None) => match self.stream.recv.poll_data(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Ok(Some(data))) => {
                        self.received = (self.received.0 + data.len() as u64, self.received.1 + 1);
                        if self.received.0 > MAX_CONNECT_BYTES || self.received.1 > MAX_CONNECT_CHUNKS {
                            return Err(self.stream.abort(Code::H3_EXCESSIVE_LOAD));
                        }
                        self.input = data;
                    }
                    Poll::Ready(Ok(None)) if self.capsules.at_boundary() => {
                        self.peer_finished = true;
                        self.end(Ok((0, String::new())));
                    }
                    Poll::Ready(Ok(None)) => return Err(self.stream.abort(Code::H3_MESSAGE_ERROR)),
                    Poll::Ready(Err(error)) => return Err(error),
                },
            }
        }
        Ok(())
    }

    /// The first ending stands.
    pub(crate) fn end(&self, ending: Result<(u32, String), Error>) {
        self.phase.send_if_modified(|phase| {
            !phase.is_ended() && {
                *phase = Phase::Ended(ending);
                true
            }
        });
    }
}

const EARLY_WAIT: Duration = Duration::from_secs(5);
const MAX_EARLY: usize = 64;
pub(super) const STREAM_QUEUE: usize = 32;
pub(super) const DATAGRAM_QUEUE: usize = 256;

/// A peer's session stream before it reaches its session.
pub(crate) struct Unrouted {
    pub(crate) stream: noq::RecvStream,
    /// Bytes that followed the stream header in its first chunk.
    pub(crate) first: Bytes,
    pub(crate) charge: Charge,
}

impl Unrouted {
    fn refuse(mut self, code: Code) {
        let _ = self.stream.stop(code.into());
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    active: Option<Active>,
    early: VecDeque<(Instant, u64, Unrouted)>,
    /// Sessions at or below this ID have ended.
    gone_through: Option<u64>,
    /// The close every session gets once the server shuts down.
    shutdown: Option<(u32, String)>,
    /// A sessions-only connection stays open until then, so the CLOSE of a session this side ended arrives first.
    pub(crate) linger: Option<Instant>,
    /// Whether the connection carried a session, and served anything else.
    carried: bool,
    pub(crate) served: bool,
    /// CONNECT streams and cancelled streams the driver has yet to take.
    handed: (Vec<Connect>, Vec<PendingReset>),
    ended: bool,
}

struct Active {
    id: u64,
    streams: mpsc::Sender<RecvStream>,
    datagrams: mpsc::Sender<Bytes>,
    phase: watch::Receiver<Phase>,
    /// This side's close, from the application or the shutdown; the first one stands.
    close: Option<(u32, String)>,
}

pub(super) type Queues = (mpsc::Receiver<RecvStream>, mpsc::Receiver<Bytes>);

impl Registry {
    pub(crate) fn stream(&mut self, session: u64, stream: Unrouted) {
        match &self.active {
            Some(active) if active.id == session => {
                let stream = RecvStream::new(stream, active.phase.clone());
                match active.streams.try_send(stream) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(mut stream)) => stream.stop(Code::WT_BUFFERED_STREAM_REJECTED),
                    Err(mpsc::error::TrySendError::Closed(mut stream)) => stream.stop(Code::WT_SESSION_GONE),
                }
            }
            _ if self.gone_through.is_some_and(|gone| session <= gone) => stream.refuse(Code::WT_SESSION_GONE),
            _ if self.early.len() < MAX_EARLY => self.early.push_back((Instant::now() + EARLY_WAIT, session, stream)),
            _ => stream.refuse(Code::WT_BUFFERED_STREAM_REJECTED),
        }
    }

    /// Datagrams are unreliable: one for no current session, or over the queue, is dropped.
    pub(crate) fn datagram(&mut self, session: u64, payload: Bytes) {
        if let Some(active) = self.active(session) {
            let _ = active.datagrams.try_send(payload);
        }
    }

    /// Refuses streams that waited too long for their session.
    pub(crate) fn expire(&mut self, now: Instant) {
        while self.early.front().is_some_and(|(deadline, ..)| *deadline <= now) {
            let (_, _, stream) = self.early.pop_front().expect("an early stream");
            stream.refuse(Code::WT_BUFFERED_STREAM_REJECTED);
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.early.front().map(|(deadline, ..)| *deadline)
    }

    /// A connection that carried sessions and served nothing else ends with its last session.
    pub(crate) fn only_sessions(&self) -> bool {
        self.carried && !self.served
    }

    /// Server: every session, current or yet to come, ends with this close.
    pub(crate) fn shutdown(&mut self, code: u32, reason: &str) {
        self.shutdown = Some((code, reason.into()));
        if let Some(active) = &mut self.active {
            active.close.get_or_insert_with(|| (code, reason.into()));
        }
    }

    /// The connection ended, and so do the session's streams and datagrams; returns what the driver had yet to take.
    pub(super) fn end(&mut self) -> (Vec<Connect>, Vec<PendingReset>) {
        (self.active, self.ended) = (None, true);
        self.early.clear();
        self.take()
    }

    pub(super) fn take(&mut self) -> (Vec<Connect>, Vec<PendingReset>) {
        std::mem::take(&mut self.handed)
    }

    /// Leaves a cancelled stream for the driver, or resets it at once if the connection ended.
    pub(super) fn defer(&mut self, mut reset: PendingReset) {
        if self.ended {
            reset.abandon();
        } else {
            self.handed.1.push(reset);
        }
    }

    /// Whether a session may start: the connection has none and has not ended.
    pub(super) fn vacant(&self) -> bool {
        self.active.is_none() && !self.ended
    }

    /// Makes `connect` the connection's session, which must be vacant, for the driver to run.
    pub(super) fn register(&mut self, connect: Connect) -> Queues {
        let (streams, stream_queue) = mpsc::channel(STREAM_QUEUE);
        let (datagrams, datagram_queue) = mpsc::channel(DATAGRAM_QUEUE);
        let (id, phase, close) = (connect.id, connect.phase.subscribe(), self.shutdown.clone());
        self.active = Some(Active { id, streams, datagrams, phase, close });
        self.carried = true;
        self.handed.0.push(connect);
        for (deadline, session, stream) in std::mem::take(&mut self.early) {
            if session == id {
                self.stream(session, stream);
            } else {
                self.early.push_back((deadline, session, stream));
            }
        }
        (stream_queue, datagram_queue)
    }

    pub(super) fn request_close(&mut self, id: u64, code: u32, reason: &str) {
        if let Some(active) = self.active(id) {
            active.close.get_or_insert_with(|| (code, reason.into()));
        }
    }

    pub(super) fn requested_close(&mut self, id: u64) -> Option<(u32, String)> {
        self.active(id).and_then(|active| active.close.clone())
    }

    /// The session is gone for routing; later streams naming it are refused.
    pub(super) fn unregister(&mut self, id: u64) {
        if self.active(id).is_some() {
            self.active = None;
        }
        self.gone_through = self.gone_through.max(Some(id));
    }

    fn active(&mut self, id: u64) -> Option<&mut Active> {
        self.active.as_mut().filter(|active| active.id == id)
    }
}

/// How long a cancelled stream may take to deliver its association header.
const RESET_DEADLINE: Duration = Duration::from_secs(10);
const LANE_CANCELLED: WtCode = WtCode(0);

/// A peer's stream in a session: noq's chunks as they arrived. Dropping it unread cancels the lane.
pub struct RecvStream {
    stream: noq::RecvStream,
    first: Bytes,
    done: bool,
    session: watch::Receiver<Phase>,
    _charge: Charge,
}

impl RecvStream {
    pub(super) fn new(unrouted: Unrouted, session: watch::Receiver<Phase>) -> Self {
        let Unrouted { stream, first, charge } = unrouted;
        Self { stream, first, done: false, session, _charge: charge }
    }

    /// The next chunk, `None` at the stream's end.
    pub async fn read_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let mut chunk = [Bytes::new()];
        let read = self.read_chunks(&mut chunk).await?;
        Ok(read.map(|_| std::mem::take(&mut chunk[0])))
    }

    /// Fills `chunks` with the next chunks under one lock of the connection: how many, `None` at the end.
    pub async fn read_chunks(&mut self, chunks: &mut [Bytes]) -> Result<Option<usize>, Error> {
        let (first, stream) = (&mut self.first, &mut self.stream);
        let read = async {
            if first.is_empty() {
                return stream.read_many_chunks(chunks).await;
            }
            chunks[0] = std::mem::take(first);
            Ok(Some(1))
        };
        let Ok(read) = unless_ended(&self.session, read).await else {
            self.stop(Code::WT_SESSION_GONE);
            return Err(Error::Refused);
        };
        self.done |= !matches!(read, Ok(Some(_)));
        Ok(read?)
    }

    /// Stops the stream with `code`, or once its session ended with WT_SESSION_GONE.
    pub fn stop(&mut self, code: Code) {
        if !std::mem::replace(&mut self.done, true) {
            let code = if self.session.borrow().is_ended() { Code::WT_SESSION_GONE } else { code };
            let _ = self.stream.stop(code.into());
        }
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        self.stop(LANE_CANCELLED.to_http());
    }
}

/// A stream this side opened; dropped unfinished it resets past its header, which RESET_STREAM_AT peers still get.
pub struct SendStream {
    lane: Option<Lane>,
    session: watch::Receiver<Phase>,
    shared: Arc<Shared>,
}

impl SendStream {
    pub(super) async fn open(shared: &Arc<Shared>, id: u64, session: watch::Receiver<Phase>) -> Result<Self, Error> {
        let stream = shared.quic.open_uni().await?;
        let lane = Lane {
            stream,
            header: Header::new(frame::WEBTRANSPORT_STREAM, id),
            code: LANE_CANCELLED.to_http(),
        };
        let mut opened = Self { lane: Some(lane), session, shared: shared.clone() };
        let lane = opened.lane.as_mut().expect("open stream");
        poll_fn(|cx| poll_write_header(&mut lane.header, &mut lane.stream, cx)).await?;
        Ok(opened)
    }

    /// The stream, refused once its session ended.
    fn stream(&mut self) -> Result<(&mut noq::SendStream, &watch::Receiver<Phase>), Error> {
        if self.session.borrow().is_ended() {
            return Err(Error::Refused);
        }
        Ok((&mut self.lane.as_mut().expect("open stream").stream, &self.session))
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let (stream, session) = self.stream()?;
        Ok(unless_ended(session, stream.write_all(bytes)).await??)
    }

    /// noq keeps `chunk` uncopied, charged by length, so it must not pin a larger buffer.
    pub async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Error> {
        let mut chunks = [chunk];
        let mut unwritten = &mut chunks[..];
        while !unwritten.is_empty() {
            let (stream, session) = self.stream()?;
            unless_ended(session, stream.write_leased_chunks(&mut unwritten)).await??;
        }
        Ok(())
    }

    /// Refused once the session ended, when the stream is reset instead.
    pub fn finish(mut self) -> Result<(), Error> {
        self.stream()?;
        let mut lane = self.lane.take().expect("open stream");
        lane.stream
            .finish()
            .map_err(|_| Error::Stopped(Code::H3_REQUEST_CANCELLED))
    }

    pub fn reset(mut self, code: WtCode) {
        self.lane.as_mut().expect("open stream").code = code.to_http();
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let Some(mut lane) = self.lane.take() else { return };
        if self.session.borrow().is_ended() {
            lane.code = Code::WT_SESSION_GONE;
        }
        if lane.header.is_written() {
            lane.cancel();
        } else {
            let deadline = Instant::now() + RESET_DEADLINE;
            self.shared
                .sessions()
                .defer(PendingReset { lane, deadline, charge: None });
            self.shared.wake();
        }
    }
}

struct Lane {
    stream: noq::SendStream,
    header: Header,
    code: Code,
}

impl Lane {
    /// Resets after the association header, with plain RESET_STREAM for peers without reliable reset.
    fn cancel(&mut self) {
        let reliable = noq::VarInt::from_u32(u32::from(self.header.len()));
        if let Err(noq::ResetStreamAtError::Unsupported) = self.stream.reset_at(reliable, self.code.into()) {
            let _ = self.stream.reset(self.code.into());
        }
    }
}

/// A cancelled stream whose association header is still being written; the driver completes it.
pub(crate) struct PendingReset {
    lane: Lane,
    pub(crate) deadline: Instant,
    pub(crate) charge: Option<Charge>,
}

impl PendingReset {
    /// Resets once the header is out; `true` when the stream is done with.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, now: Instant) -> bool {
        if now >= self.deadline {
            self.abandon();
            return true;
        }
        match poll_write_header(&mut self.lane.header, &mut self.lane.stream, cx) {
            Poll::Pending => false,
            Poll::Ready(written) => {
                if written.is_ok() {
                    self.lane.cancel();
                }
                true
            }
        }
    }

    /// Resets without the association: never a FIN.
    pub(crate) fn abandon(&mut self) {
        let _ = self.lane.stream.reset(self.lane.code.into());
    }
}
