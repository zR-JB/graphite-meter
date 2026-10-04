//! WebTransport over HTTP/3, one session per connection in either dialect: the session registry,
//! stream association, datagrams, capsules, reliable resets and the close sequence.
use crate::{
    capsule,
    charge::Charge,
    client::SendRequest,
    code::{Code, WtCode},
    connection::Shared,
    error::Error,
    frame::{self, Header},
    settings::Dialect,
    stream::RequestStream,
    varint,
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
const REORDERING: Duration = Duration::from_secs(5);
const MAX_PENDING: usize = 64;
const STREAM_QUEUE: usize = 32;
const DATAGRAM_QUEUE: usize = 256;
const CLOSE_DRAIN: Duration = Duration::from_secs(1);
const RESET_DEADLINE: Duration = Duration::from_secs(10);
/// Control data a peer may send on its CONNECT stream.
const MAX_CONNECT_BYTES: u64 = 1024 * 1024;
const MAX_CONNECT_CHUNKS: u64 = 1024;
const LANE_CANCELLED: WtCode = WtCode(0);

/// The connection's one session, and peer streams that arrived before theirs.
#[derive(Default)]
pub(crate) struct Registry {
    active: Option<Active>,
    pending: VecDeque<(Instant, u64, RecvStream)>,
    /// Sessions at or below this ID have ended.
    gone_through: Option<u64>,
    /// The close every session gets once the server shuts the connection down.
    shutdown: Option<(u32, String)>,
    /// A sessions-only connection stays open until then, so the CLOSE of a session this side ended
    /// reaches the peer first.
    pub(crate) linger: Option<Instant>,
    /// Whether the connection carried a session, and served anything else.
    pub(crate) carried: bool,
    pub(crate) served: bool,
    /// The connection ended, so no session starts.
    ended: bool,
}

struct Active {
    id: u64,
    streams: mpsc::Sender<RecvStream>,
    datagrams: mpsc::Sender<Bytes>,
    /// This side's close, from the application or the shutdown; the first one stands.
    close: Option<(u32, String)>,
}

impl Registry {
    pub(crate) fn stream(&mut self, session: u64, mut stream: RecvStream) {
        match &self.active {
            Some(active) if active.id == session => match active.streams.try_send(stream) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(mut stream)) => stream.stop(Code::WT_BUFFERED_STREAM_REJECTED),
                Err(mpsc::error::TrySendError::Closed(mut stream)) => stream.stop(Code::WT_SESSION_GONE),
            },
            _ if self.gone_through.is_some_and(|gone| session <= gone) => stream.stop(Code::WT_SESSION_GONE),
            _ if self.pending.len() < MAX_PENDING => {
                self.pending.push_back((Instant::now() + REORDERING, session, stream))
            }
            _ => stream.stop(Code::WT_BUFFERED_STREAM_REJECTED),
        }
    }

    /// Datagrams are unreliable: one for no current session, or over the queue, is dropped.
    pub(crate) fn datagram(&mut self, session: u64, payload: Bytes) {
        if let Some(active) = self.active(session) {
            let _ = active.datagrams.try_send(payload);
        }
    }

    /// Refuses streams that waited too long for their session; returns the next deadline.
    pub(crate) fn expire(&mut self, now: Instant) -> Option<Instant> {
        while self.pending.front().is_some_and(|(deadline, ..)| *deadline <= now) {
            let (_, _, mut stream) = self.pending.pop_front().expect("pending stream");
            stream.stop(Code::WT_BUFFERED_STREAM_REJECTED);
        }
        self.pending.front().map(|(deadline, ..)| *deadline)
    }

    /// Server: every session, current or yet to come, ends with this close.
    pub(crate) fn shutdown(&mut self, code: u32, reason: &str) {
        self.shutdown = Some((code, reason.into()));
        if let Some(active) = &mut self.active {
            active.close.get_or_insert_with(|| (code, reason.into()));
        }
    }

    /// The connection ended: so do the session's streams and datagrams, and no session follows.
    pub(crate) fn end(&mut self) {
        (self.active, self.ended) = (None, true);
        self.pending.clear();
    }

    fn register(&mut self, id: u64) -> Option<(mpsc::Receiver<RecvStream>, mpsc::Receiver<Bytes>)> {
        if self.active.is_some() || self.ended {
            return None;
        }
        let (streams, stream_receiver) = mpsc::channel(STREAM_QUEUE);
        let (datagrams, datagram_receiver) = mpsc::channel(DATAGRAM_QUEUE);
        let close = self.shutdown.clone();
        self.active = Some(Active { id, streams, datagrams, close });
        self.carried = true;
        for (deadline, session, stream) in std::mem::take(&mut self.pending) {
            if session == id {
                self.stream(session, stream);
            } else {
                self.pending.push_back((deadline, session, stream));
            }
        }
        Some((stream_receiver, datagram_receiver))
    }

    fn active(&mut self, id: u64) -> Option<&mut Active> {
        self.active.as_mut().filter(|active| active.id == id)
    }

    fn request_close(&mut self, id: u64, code: u32, reason: &str) {
        if let Some(active) = self.active(id) {
            active.close.get_or_insert_with(|| (code, reason.into()));
        }
    }

    fn unregister(&mut self, id: u64) {
        if self.active(id).is_some() {
            self.active = None;
        }
        self.gone_through = self.gone_through.max(Some(id));
    }
}

/// A session's end, which its streams watch: the drafts end them with it, and send nothing more.
type Ended = watch::Receiver<Option<Result<(u32, String), Error>>>;

/// `work`'s outcome, refused once the session ended, even while `work` waits: webtransport-go's
/// closeWithSession wakes a blocked read or write so. Work that is ready at once watches nothing,
/// so a busy stream never waits on the session.
async fn unless_ended<T>(session: Option<&Ended>, work: impl Future<Output = T>) -> Result<T, Error> {
    let gone = session.is_some_and(|session| session.borrow().is_some());
    tokio::select! {
        biased;
        done = work, if !gone => Ok(done),
        Some(()) = async { session?.clone().wait_for(Option::is_some).await.ok().map(drop) } => Err(Error::Refused),
    }
}

/// A peer's stream in a session: noq's chunks as they arrived. Dropping it unread cancels the lane;
/// once its session ended, reads are refused, a waiting one too, and it is stopped with
/// WT_SESSION_GONE.
pub struct RecvStream {
    stream: noq::RecvStream,
    /// Bytes that followed the stream header in its first chunk.
    first: Bytes,
    done: bool,
    /// Its session's end, from when the session hands it over.
    session: Option<Ended>,
    _charge: Charge,
}

impl RecvStream {
    pub(crate) fn new(stream: noq::RecvStream, first: Bytes, charge: Charge) -> Self {
        Self { stream, first, done: false, session: None, _charge: charge }
    }

    /// The next chunk, `None` at the stream's end.
    pub async fn read_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let (first, stream) = (&mut self.first, &mut self.stream);
        let read = async {
            match std::mem::take(first) {
                first if first.is_empty() => stream.read_chunk(usize::MAX).await,
                first => Ok(Some(first)),
            }
        };
        let Ok(chunk) = unless_ended(self.session.as_ref(), read).await else {
            self.stop(Code::WT_SESSION_GONE);
            return Err(Error::Refused);
        };
        self.done |= !matches!(chunk, Ok(Some(_)));
        Ok(chunk?)
    }

    /// Stops the stream with `code`, or once its session ended with WT_SESSION_GONE.
    pub fn stop(&mut self, code: Code) {
        if !std::mem::replace(&mut self.done, true) {
            let gone = self.session.as_ref().is_some_and(|session| session.borrow().is_some());
            let code = if gone { Code::WT_SESSION_GONE } else { code };
            let _ = self.stream.stop(code.into());
        }
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        self.stop(LANE_CANCELLED.to_http());
    }
}

/// A stream this side opened in a session. Dropping it resets after its association header, which
/// peers that support RESET_STREAM_AT still receive; it is never finished by accident. Once its
/// session ended, writes are refused, a waiting one too, and the reset carries WT_SESSION_GONE.
pub struct SendStream {
    /// The reset it gets when dropped, until it is finished.
    lane: Option<PendingReset>,
    shared: Arc<Shared>,
    session: Ended,
}

impl SendStream {
    async fn open(shared: &Arc<Shared>, id: u64, session: Ended) -> Result<Self, Error> {
        let lane = PendingReset {
            stream: shared.quic.open_uni().await?,
            header: Header::new(frame::WEBTRANSPORT_STREAM, id),
            code: LANE_CANCELLED.to_http(),
            deadline: Instant::now(),
            _charge: None,
        };
        let mut opened = Self { lane: Some(lane), shared: shared.clone(), session };
        let lane = opened.lane.as_mut().expect("open stream");
        poll_fn(|cx| lane.header.poll_write(&mut lane.stream, cx)).await?;
        Ok(opened)
    }

    /// The stream and its session's end, refused once the session ended.
    fn stream(&mut self) -> Result<(&mut noq::SendStream, &Ended), Error> {
        if self.session.borrow().is_some() {
            return Err(Error::Refused);
        }
        Ok((&mut self.lane.as_mut().expect("open stream").stream, &self.session))
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let (stream, session) = self.stream()?;
        Ok(unless_ended(Some(session), stream.write_all(bytes)).await??)
    }

    /// Noq keeps `chunk` uncopied, charged by length, so it must not pin a larger buffer.
    pub async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Error> {
        let mut chunks = [chunk];
        let mut unwritten = &mut chunks[..];
        while !unwritten.is_empty() {
            let (stream, session) = self.stream()?;
            unless_ended(Some(session), stream.write_leased_chunks(&mut unwritten)).await??;
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
        if self.session.borrow().is_some() {
            lane.code = Code::WT_SESSION_GONE;
        }
        if lane.header.is_written() {
            lane.cancel();
        } else {
            lane.deadline = Instant::now() + RESET_DEADLINE;
            self.shared.defer_reset(lane);
        }
    }
}

/// A cancelled stream whose association header is still being written; the driver completes it.
pub(crate) struct PendingReset {
    stream: noq::SendStream,
    header: Header,
    code: Code,
    pub(crate) deadline: Instant,
    pub(crate) _charge: Option<Charge>,
}

impl PendingReset {
    /// Resets once the header is out; `true` when the stream is done with.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, now: Instant) -> bool {
        if now >= self.deadline {
            self.abandon();
            return true;
        }
        let Poll::Ready(written) = self.header.poll_write(&mut self.stream, cx) else {
            return false;
        };
        if written.is_ok() {
            self.cancel();
        }
        true
    }

    /// Resets without the association: never a FIN.
    pub(crate) fn abandon(&mut self) {
        let _ = self.stream.reset(self.code.into());
    }

    pub(crate) fn cancel(&mut self) {
        let reliable = noq::VarInt::from_u32(u32::from(self.header.len()));
        if let Err(noq::ResetStreamAtError::Unsupported) = self.stream.reset_at(reliable, self.code.into()) {
            let _ = self.stream.reset(self.code.into());
        }
    }
}

/// A session's CONNECT stream once its head is out: the connection's driver reads the peer's
/// capsules and runs the close, so every session ends with one whatever its handle does.
pub(crate) struct Connect {
    id: u64,
    stream: RequestStream,
    capsules: capsule::Reader,
    input: Bytes,
    bytes: u64,
    chunks: u64,
    ended: watch::Sender<Option<Result<(u32, String), Error>>>,
    /// Rises once the head is in QUIC's stream buffer.
    head: watch::Sender<bool>,
    /// Once the session ended: when the wait for the peer's FIN gives up.
    pub(crate) deadline: Option<Instant>,
    peer_finished: bool,
    /// The peer's CLOSE arrived, so only its FIN may follow.
    peer_closed: bool,
    /// This side ended the session, so a sessions-only connection lingers for its CLOSE.
    closed_here: bool,
}

impl Connect {
    /// Ends the session once the peer does or this side asks: CLOSE unless the peer ended it, FIN,
    /// the peer's FIN within 1 s, and only then STOP_SENDING. `true` once the stream is done with.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, now: Instant, shared: &Shared) -> bool {
        // A peer that withholds credit for the head gets no CLOSE: the drain ends it with WT_SESSION_GONE.
        let mut flushed = true;
        let failed = match self.poll_read(cx) {
            Ok(()) if self.deadline.is_none() => match self.stream.send.poll_ready(cx) {
                Poll::Pending => {
                    flushed = false;
                    None
                }
                Poll::Ready(written) => {
                    self.head.send_if_modified(|head| !std::mem::replace(head, true));
                    written.err()
                }
            },
            read => read.err(),
        };
        if let Some(error) = failed {
            self.end(Err(error));
        } else {
            let deadline = match self.deadline {
                Some(deadline) => deadline,
                None => {
                    if self.ended.borrow().is_none() {
                        let requested = shared
                            .state()
                            .sessions
                            .active(self.id)
                            .and_then(|active| active.close.clone());
                        let Some((code, reason)) = requested else { return false };
                        // Queueing replaces a frame not yet written, so an unsent head keeps its place.
                        if flushed {
                            let capsule = capsule::close(code, &reason).into();
                            self.stream.send.queue(frame::DATA, capsule);
                        }
                        self.end(Ok((code, reason)));
                        self.closed_here = true;
                    }
                    // The session is gone for routing; its CONNECT stream finishes on its own.
                    shared.state().sessions.unregister(self.id);
                    *self.deadline.insert(now + CLOSE_DRAIN)
                }
            };
            let finished = self.stream.send.poll_finish(cx).is_ready();
            if !(finished && self.peer_finished) && now < deadline {
                return false;
            }
            // Browsers drop the code of a CLOSE whose STOP_SENDING arrives first.
            self.stream.send.reset(Code::WT_SESSION_GONE);
            self.stream.recv.stop(Code::WT_SESSION_GONE);
        }
        let mut state = shared.state();
        state.sessions.unregister(self.id);
        if self.closed_here {
            state.sessions.linger = Some(now + CLOSE_DRAIN);
        }
        true
    }

    /// Reads capsules to the peer's FIN; its CLOSE, or a FIN without one, ends the session. Data
    /// after its CLOSE is H3_MESSAGE_ERROR, as the drafts require.
    fn poll_read(&mut self, cx: &mut Context<'_>) -> Result<(), Error> {
        while !self.peer_finished {
            if self.peer_closed && !self.input.is_empty() {
                return Err(self.stream.abort(Code::H3_MESSAGE_ERROR));
            }
            match self.capsules.read(&mut self.input) {
                Err(code) => return Err(self.stream.abort(code)),
                Ok(Some((code, reason))) => {
                    self.peer_closed = true;
                    self.end(Ok((code, reason)));
                }
                Ok(None) => match self.stream.recv.poll_data(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Ok(Some(data))) => {
                        (self.bytes, self.chunks) = (self.bytes + data.len() as u64, self.chunks + 1);
                        if self.bytes > MAX_CONNECT_BYTES || self.chunks > MAX_CONNECT_CHUNKS {
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
        self.ended
            .send_if_modified(|ended| ended.is_none() && ended.replace(ending).is_none());
    }
}

/// A WebTransport session. Its methods take `&self`, so lanes can share it; dropping it unclosed
/// ends the session as finished, with code 0.
pub struct Session {
    id: u64,
    shared: Arc<Shared>,
    datagram_prefix: ([u8; 8], usize),
    streams: Mutex<mpsc::Receiver<RecvStream>>,
    datagrams: Mutex<mpsc::Receiver<Bytes>>,
    ended: Ended,
    head: watch::Receiver<bool>,
    _charge: Charge,
}

/// A payload encoded for one session, reusable without copying it for every send.
pub struct PreparedDatagram<'a> {
    session: &'a Session,
    bytes: Bytes,
}

impl PreparedDatagram<'_> {
    /// Sends the payload once the queue has room, refused if the session ends while waiting.
    pub async fn send_wait(&mut self) -> Result<(), Error> {
        let sent = self.session.shared.quic.send_datagram_wait(self.bytes.clone());
        Ok(unless_ended(Some(&self.session.ended), sent).await??)
    }
}

/// State a session keeps apart from its CONNECT stream's halves: channel slots, the capsule reader
/// and both sides' close reasons.
const SESSION_BYTES: usize = size_of::<Session>()
    + size_of::<Connect>()
    + STREAM_QUEUE * size_of::<RecvStream>()
    + DATAGRAM_QUEUE * size_of::<Bytes>()
    + 4
    + 2 * capsule::MAX_REASON;

impl Session {
    /// Registers the connection's one session and hands its CONNECT stream to the driver; a second
    /// is refused with `refusal`.
    fn register(mut stream: RequestStream, refusal: Code) -> Result<Self, Error> {
        let (shared, id) = (stream.shared().clone(), stream.id());
        let registered = Charge::new(&shared.budget, SESSION_BYTES)
            .and_then(|charge| Some((charge, shared.state().sessions.register(id)?)));
        let Some((charge, (streams, datagrams))) = registered else {
            stream.abort(refusal);
            return Err(Error::Refused);
        };
        let mut prefix = [0; 8];
        let mut rest = &mut prefix[..];
        varint::put(id / 4, &mut rest);
        let prefix_len = 8 - rest.len();
        let (ended, ending) = watch::channel(None);
        let (head, written) = watch::channel(false);
        let connect = Connect {
            id,
            stream,
            capsules: capsule::Reader::default(),
            input: Bytes::new(),
            bytes: 0,
            chunks: 0,
            ended,
            head,
            deadline: None,
            peer_finished: false,
            peer_closed: false,
            closed_here: false,
        };
        // Without a driver the connection is gone, and so is the stream.
        let _ = shared.connects.send(connect);
        Ok(Self {
            id,
            shared,
            datagram_prefix: (prefix, prefix_len),
            streams: Mutex::new(streams),
            datagrams: Mutex::new(datagrams),
            ended: ending,
            head: written,
            _charge: charge,
        })
    }

    /// Server: accepts an authorized WebTransport CONNECT, answering with `headers` either way. A peer
    /// that has not shown WebTransport, HTTP datagrams and the datagram transport parameter within
    /// 5 s gets 400; a second session on the connection gets H3_REQUEST_REJECTED, as from Go.
    pub async fn accept(mut stream: RequestStream, headers: http::HeaderMap) -> Result<Self, Error> {
        let shared = stream.shared().clone();
        let mut response = http::Response::new(());
        *response.headers_mut() = headers;
        let Some(dialect) = dialect(&shared, SETTINGS_WAIT).await.ok().flatten() else {
            *response.status_mut() = http::StatusCode::BAD_REQUEST;
            // A peer that withholds credit cannot hold the refusal.
            let answer = async {
                stream.send.send_response(response).await?;
                stream.send.finish().await
            };
            match tokio::time::timeout(RESET_DEADLINE, answer).await {
                Ok(answered) => answered?,
                Err(_) => stream.send.reset(Code::H3_REQUEST_CANCELLED),
            }
            return Err(Error::Refused);
        };
        if dialect == Dialect::Draft02 {
            // Draft 02 requires it; Go omits it.
            response
                .headers_mut()
                .insert("sec-webtransport-http3-draft", http::HeaderValue::from_static("draft02"));
        }
        // The driver writes it before any capsule.
        stream.send.queue_response(response)?;
        Self::register(stream, Code::H3_REQUEST_REJECTED)
    }

    /// Client: opens a session, speaking the server's dialect, with the response that accepted it.
    /// A refusal returns its response; after the server's GOAWAY none is sent (RFC 9114 §5.2). The
    /// server's SETTINGS are awaited as long as the caller waits, as webtransport-go awaits them.
    pub async fn connect(
        requests: &SendRequest,
        request: http::Request<()>,
    ) -> Result<Result<(Self, http::Response<()>), http::Response<()>>, Error> {
        let shared = &requests.0;
        let dialect = dialect(shared, Duration::MAX)
            .await?
            .filter(|_| shared.peer.borrow().is_some_and(|peer| peer.connect_protocol));
        if shared.going_away() {
            return Err(Error::GoingAway);
        }
        let (mut parts, ()) = request.into_parts();
        parts.method = http::Method::CONNECT;
        let protocol = match dialect.ok_or(Error::NoWebTransport)? {
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
        Self::register(stream, Code::H3_REQUEST_CANCELLED).map(|session| Ok((session, response)))
    }

    /// The next stream the peer opened in this session; `None` once the session ended, also with
    /// its connection, and [`Self::closed`] says how.
    pub async fn accept_uni(&self) -> Option<RecvStream> {
        let mut stream = self.streams.lock().await.recv().await?;
        stream.session = Some(self.ended.clone());
        Some(stream)
    }

    /// The next datagram in this session; `None` once the session ended, as for [`Self::accept_uni`].
    pub async fn read_datagram(&self) -> Option<Bytes> {
        self.datagrams.lock().await.recv().await
    }

    /// Refused once the session ended: the drafts allow no new stream after its CLOSE.
    /// A stream waits for the head: its data would wait unread for the response and could take the
    /// connection credit the response needs.
    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        let mut head = self.head.clone();
        unless_ended(Some(&self.ended), head.wait_for(|written| *written))
            .await?
            .map_err(|_| Error::Refused)?;
        SendStream::open(&self.shared, self.id, self.ended.clone()).await
    }

    /// A datagram's bytes, refused once the session ended: the drafts send none after it.
    fn datagram(&self, payload: &[u8]) -> Result<Bytes, Error> {
        if self.ended.borrow().is_some() {
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

    /// Resolves once the session ends: with the peer's CLOSE code and reason, code 0 when the peer
    /// finished without one, this side's close when it ended the session first, or the error its
    /// connection ended with.
    pub async fn closed(&self) -> Result<(u32, String), Error> {
        let mut ended = self.ended.clone();
        match ended.wait_for(Option::is_some).await.map(|ended| ended.clone()) {
            Ok(Some(ending)) => ending,
            // The stream reached no driver: the connection had ended.
            _ => Err(self.shared.close_error()),
        }
    }

    /// Ends the session with `code` and `reason` unless it already ended, and returns once its
    /// CONNECT stream is done: CLOSE and FIN, then the peer's FIN within 1 s, and only then
    /// STOP_SENDING. Browsers report that order's code and reason; Go's all-at-once close reads as
    /// a failure.
    pub async fn close(&self, code: u32, reason: &str) {
        self.request_close(code, reason);
        let mut ended = self.ended.clone();
        while ended.changed().await.is_ok() {}
    }

    fn request_close(&self, code: u32, reason: &str) {
        let mut state = self.shared.state();
        state.sessions.request_close(self.id, code, reason);
        state.wake();
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

/// The WebTransport rules applied to the peer's SETTINGS, awaited for `wait`, or the error the
/// connection ended with first.
async fn dialect(shared: &Shared, wait: Duration) -> Result<Option<Dialect>, Error> {
    let mut peer = shared.peer.subscribe();
    let settled = peer.wait_for(|peer| peer.is_some() || shared.quic.close_reason().is_some());
    let peer = tokio::time::timeout(wait, settled).await.map_err(|_| Error::TimedOut)?;
    let peer = peer.ok().and_then(|peer| *peer).ok_or_else(|| shared.close_error())?;
    Ok(peer.webtransport(shared.quic.max_datagram_size().is_some()))
}
