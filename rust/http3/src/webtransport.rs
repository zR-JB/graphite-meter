//! WebTransport over HTTP/3, one session per connection in either dialect: the session registry,
//! stream association, datagrams, capsules, reliable resets and the close sequence.
use crate::{
    capsule::{self, Capsule},
    charge::Charge,
    client::SendRequest,
    code::{Code, WtCode},
    connection::Shared,
    error::Error,
    fields, frame,
    settings::Dialect,
    stream::{self, RecvHalf, RequestStream, SendHalf},
    varint,
};
use bytes::Bytes;
use std::{
    collections::VecDeque,
    future::{Future, poll_fn},
    pin::pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc},
    time::Instant,
};

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
    active: Option<Queues>,
    pending: VecDeque<(Instant, u64, RecvStream)>,
    /// Sessions at or below this ID have ended.
    gone_through: Option<u64>,
    /// Whether the connection carried a session, and served anything else.
    pub(crate) carried: bool,
    pub(crate) served: bool,
}

struct Queues {
    id: u64,
    streams: mpsc::Sender<RecvStream>,
    datagrams: mpsc::Sender<Bytes>,
}

impl Registry {
    pub(crate) fn stream(&mut self, session: u64, mut stream: RecvStream) {
        match &self.active {
            Some(queues) if queues.id == session => {
                if let Err(refused) = queues.streams.try_send(stream) {
                    refused.into_inner().stop(Code::WT_BUFFERED_STREAM_REJECTED);
                }
            }
            _ if self.gone_through.is_some_and(|gone| session <= gone) => stream.stop(Code::WT_SESSION_GONE),
            _ if self.pending.len() < MAX_PENDING => {
                self.pending.push_back((Instant::now() + REORDERING, session, stream))
            }
            _ => stream.stop(Code::WT_BUFFERED_STREAM_REJECTED),
        }
    }

    /// Datagrams are unreliable: one for no current session, or over the queue, is dropped.
    pub(crate) fn datagram(&self, session: u64, payload: Bytes) {
        if let Some(queues) = self.active.as_ref().filter(|queues| queues.id == session) {
            let _ = queues.datagrams.try_send(payload);
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

    fn register(&mut self, id: u64) -> Option<(mpsc::Receiver<RecvStream>, mpsc::Receiver<Bytes>)> {
        if self.active.is_some() {
            return None;
        }
        let (streams, stream_receiver) = mpsc::channel(STREAM_QUEUE);
        let (datagrams, datagram_receiver) = mpsc::channel(DATAGRAM_QUEUE);
        self.active = Some(Queues { id, streams, datagrams });
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

    fn unregister(&mut self, id: u64) {
        self.active = None;
        self.gone_through = self.gone_through.max(Some(id));
    }
}

/// A peer's stream in a session: noq's chunks as they arrived. Dropping it unread cancels the lane.
pub struct RecvStream {
    stream: noq::RecvStream,
    /// Bytes that followed the stream header in its first chunk.
    first: Bytes,
    done: bool,
    _charge: Charge,
}

impl RecvStream {
    pub(crate) fn new(stream: noq::RecvStream, first: Bytes, charge: Charge) -> Self {
        Self {
            stream,
            first,
            done: false,
            _charge: charge,
        }
    }

    pub fn poll_chunk(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<Bytes>, Error>> {
        if !self.first.is_empty() {
            return Poll::Ready(Ok(Some(std::mem::take(&mut self.first))));
        }
        let chunk = ready!(pin!(self.stream.read_chunk(usize::MAX)).poll(cx));
        self.done |= !matches!(chunk, Ok(Some(_)));
        Poll::Ready(chunk.map_err(Error::from))
    }

    pub async fn read_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        poll_fn(|cx| self.poll_chunk(cx)).await
    }

    pub fn stop(&mut self, code: Code) {
        if !std::mem::replace(&mut self.done, true) {
            let _ = self.stream.stop(code.into());
        }
    }

    pub fn id(&self) -> u64 {
        self.stream.id().into()
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        self.stop(LANE_CANCELLED.to_http());
    }
}

/// A stream this side opened in a session. Dropping it resets after its association header, which
/// peers that support RESET_STREAM_AT still receive; it is never finished by accident.
pub struct SendStream {
    stream: Option<noq::SendStream>,
    header: [u8; 16],
    header_end: u8,
    written: u8,
    code: WtCode,
    shared: Arc<Shared>,
}

impl SendStream {
    async fn open(shared: &Arc<Shared>, session: u64) -> Result<Self, Error> {
        let mut header = [0; 16];
        let mut rest = &mut header[..];
        varint::put(frame::WEBTRANSPORT_STREAM, &mut rest);
        varint::put(session, &mut rest);
        let header_end = (16 - rest.len()) as u8;
        let stream = shared.quic.open_uni().await?;
        let mut opened = Self {
            stream: Some(stream),
            header,
            header_end,
            written: 0,
            code: LANE_CANCELLED,
            shared: shared.clone(),
        };
        while opened.written < opened.header_end {
            let header = &opened.header[usize::from(opened.written)..usize::from(opened.header_end)];
            opened.written += opened.stream.as_mut().expect("open stream").write(header).await? as u8;
        }
        Ok(opened)
    }

    fn stream(&mut self) -> &mut noq::SendStream {
        self.stream.as_mut().expect("open stream")
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        Ok(self.stream().write_all(bytes).await?)
    }

    /// Passes owned bytes to noq without a copy here.
    pub async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Error> {
        Ok(self.stream().write_chunk(chunk).await?)
    }

    pub fn finish(mut self) -> Result<(), Error> {
        let mut stream = self.stream.take().expect("open stream");
        stream.finish().map_err(|_| Error::Stopped(Code::H3_REQUEST_CANCELLED))
    }

    pub fn reset(mut self, code: WtCode) {
        self.code = code;
    }

    pub fn id(&self) -> u64 {
        self.stream.as_ref().expect("open stream").id().into()
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let Some(stream) = self.stream.take() else { return };
        let mut pending = PendingReset {
            stream,
            header: self.header,
            header_end: self.header_end,
            written: self.written,
            code: self.code.to_http(),
            deadline: Instant::now() + RESET_DEADLINE,
            _charge: None,
        };
        if pending.written == pending.header_end {
            pending.cancel();
        } else {
            self.shared.defer_reset(pending);
        }
    }
}

/// A cancelled stream whose association header is still being written; the driver completes it.
pub(crate) struct PendingReset {
    stream: noq::SendStream,
    header: [u8; 16],
    header_end: u8,
    written: u8,
    code: Code,
    deadline: Instant,
    pub(crate) _charge: Option<Charge>,
}

impl PendingReset {
    /// Resets once the header is out; `true` when the stream is done with.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, now: Instant) -> bool {
        if now >= self.deadline {
            self.abandon();
            return true;
        }
        while self.written < self.header_end {
            let header = &self.header[usize::from(self.written)..usize::from(self.header_end)];
            match pin!(self.stream.write(header)).poll(cx) {
                Poll::Pending => return false,
                Poll::Ready(Ok(written)) => self.written += written as u8,
                Poll::Ready(Err(_)) => return true,
            }
        }
        self.cancel();
        true
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Resets without the association: never a FIN.
    pub(crate) fn abandon(&mut self) {
        let _ = self.stream.reset(self.code.into());
    }

    pub(crate) fn cancel(&mut self) {
        let reliable = noq::VarInt::from_u32(u32::from(self.header_end));
        if let Err(noq::ResetStreamAtError::Unsupported) = self.stream.reset_at(reliable, self.code.into()) {
            let _ = self.stream.reset(self.code.into());
        }
    }
}

/// A WebTransport session on its CONNECT stream. Its methods take `&self`, so lanes can share it.
pub struct Session {
    id: u64,
    shared: Arc<Shared>,
    datagram_prefix: ([u8; 8], usize),
    send: Mutex<SendHalf>,
    connect: Mutex<Connect>,
    streams: Mutex<mpsc::Receiver<RecvStream>>,
    datagrams: Mutex<mpsc::Receiver<Bytes>>,
    _charge: Charge,
}

struct Connect {
    recv: RecvHalf,
    capsules: capsule::Reader,
    input: Bytes,
    bytes: u64,
    chunks: u64,
    /// The peer's CLOSE, or code 0 once its side finished without one.
    closed: Option<(u32, String)>,
}

/// State a session keeps apart from its CONNECT stream: channel slots and the reader.
const SESSION_BYTES: usize = size_of::<Session>()
    + size_of::<Connect>()
    + STREAM_QUEUE * size_of::<RecvStream>()
    + DATAGRAM_QUEUE * size_of::<Bytes>()
    + 4
    + capsule::MAX_REASON;

impl Session {
    /// Registers the connection's one session; a second is refused with `refusal`.
    fn register(stream: RequestStream, refusal: Code) -> Result<Self, Error> {
        let (shared, id) = (stream.shared_arc(), stream.id());
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
        let (send, recv) = stream.split();
        let capsules = capsule::Reader::default();
        let connect = Connect {
            recv,
            capsules,
            input: Bytes::new(),
            bytes: 0,
            chunks: 0,
            closed: None,
        };
        Ok(Self {
            id,
            shared,
            datagram_prefix: (prefix, prefix_len),
            send: Mutex::new(send),
            connect: Mutex::new(connect),
            streams: Mutex::new(streams),
            datagrams: Mutex::new(datagrams),
            _charge: charge,
        })
    }

    /// Server: accepts an authorized WebTransport CONNECT. A peer that has not shown WebTransport,
    /// HTTP datagrams and the datagram transport parameter within 5 s gets 400; a second session
    /// on the connection gets H3_REQUEST_REJECTED, as from Go.
    pub async fn accept(mut stream: RequestStream) -> Result<Self, Error> {
        let shared = stream.shared_arc();
        let Some(dialect) = dialect(&shared).await else {
            let response = http::Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(())
                .expect("static");
            stream.send.send_response(response).await?;
            stream.send.finish().await?;
            return Err(Error::Refused);
        };
        let session = Self::register(stream, Code::H3_REQUEST_REJECTED)?;
        let mut response = http::Response::new(());
        if dialect == Dialect::Draft02 {
            // Draft 02 requires it; Go omits it.
            response.headers_mut().insert(
                "sec-webtransport-http3-draft",
                http::HeaderValue::from_static("draft02"),
            );
        }
        session.send.lock().await.send_response(response).await?;
        Ok(session)
    }

    /// Client: opens a session, speaking the server's dialect. A refusal returns its response.
    pub async fn connect(
        requests: &SendRequest,
        request: http::Request<()>,
    ) -> Result<Result<Self, http::Response<()>>, Error> {
        let shared = requests.shared();
        let dialect = dialect(shared)
            .await
            .filter(|_| shared.peer.borrow().is_some_and(|peer| peer.connect_protocol));
        let (mut parts, ()) = request.into_parts();
        parts.method = http::Method::CONNECT;
        let protocol = match dialect.ok_or(Error::Refused)? {
            Dialect::Draft02 => {
                parts
                    .headers
                    .insert("sec-webtransport-http3-draft02", http::HeaderValue::from_static("1"));
                "webtransport"
            }
            Dialect::Draft15 => "webtransport-h3",
        };
        let head =
            fields::encode_request(&parts, Some(protocol), shared.peer_field_limit()).map_err(|_| Error::Refused)?;
        let charges = stream::charges(&shared.budget).ok_or(Error::Refused)?;
        let (send, recv) = shared.quic.open_bi().await?;
        let limit = shared.role.field_limit();
        let stream = RequestStream::new(shared, send, recv, limit, charges);
        let session = Self::register(stream, Code::H3_REQUEST_CANCELLED)?;
        session.send.lock().await.send_request(head).await?;
        let response = session.connect.lock().await.recv.response().await?;
        Ok(if response.status().is_success() {
            Ok(session)
        } else {
            Err(response)
        })
    }

    /// The CONNECT stream's ID.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The next stream the peer opened in this session; `None` once the session is gone.
    pub async fn accept_uni(&self) -> Option<RecvStream> {
        self.streams.lock().await.recv().await
    }

    pub async fn read_datagram(&self) -> Option<Bytes> {
        self.datagrams.lock().await.recv().await
    }

    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        SendStream::open(&self.shared, self.id).await
    }

    /// The largest payload a datagram in this session can carry now.
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.shared
            .quic
            .max_datagram_size()?
            .checked_sub(self.datagram_prefix.1)
    }

    fn datagram(&self, payload: &[u8]) -> Bytes {
        let (prefix, length) = &self.datagram_prefix;
        [&prefix[..*length], payload].concat().into()
    }

    /// Sends a datagram, displacing the oldest unsent ones if the queue is full.
    pub fn send_datagram(&self, payload: &[u8]) -> Result<(), Error> {
        Ok(self.shared.quic.send_datagram(self.datagram(payload))?)
    }

    /// Sends a datagram once the queue has room.
    pub async fn send_datagram_wait(&self, payload: &[u8]) -> Result<(), Error> {
        Ok(self.shared.quic.send_datagram_wait(self.datagram(payload)).await?)
    }

    /// Resolves when the peer ends the session: its CLOSE code and reason, or code 0 when its
    /// side finished without one. Other capsules, flow control included, are ignored.
    pub async fn closed(&self) -> Result<(u32, String), Error> {
        let mut guard = self.connect.lock().await;
        let connect = &mut *guard;
        loop {
            if let Some(closed) = &connect.closed {
                return Ok(closed.clone());
            }
            match connect.capsules.read(&mut connect.input) {
                Err(code) => return Err(self.abort(connect, code)),
                Ok(Some(Capsule::Close { code, reason })) => connect.closed = Some((code, reason)),
                Ok(Some(Capsule::Drain)) => {}
                Ok(None) => match connect.recv.data().await? {
                    Some(data) => {
                        (connect.bytes, connect.chunks) = (connect.bytes + data.len() as u64, connect.chunks + 1);
                        if connect.bytes > MAX_CONNECT_BYTES || connect.chunks > MAX_CONNECT_CHUNKS {
                            return Err(self.abort(connect, Code::H3_EXCESSIVE_LOAD));
                        }
                        connect.input = data;
                    }
                    None if connect.capsules.at_boundary() => connect.closed = Some((0, String::new())),
                    None => return Err(self.abort(connect, Code::H3_MESSAGE_ERROR)),
                },
            }
        }
    }

    /// A malformed CONNECT stream ends the session in both directions.
    fn abort(&self, connect: &mut Connect, code: Code) -> Error {
        connect.recv.stop(code);
        if let Ok(mut send) = self.send.try_lock() {
            send.reset(code);
        }
        Error::Protocol(code)
    }

    /// Ends the session: CLOSE and FIN, then the peer's FIN within 1 s, and only then STOP_SENDING.
    /// Browsers report that order's code and reason; Go's all-at-once close reads as a failure.
    /// After the peer ended the session, this only finishes this side.
    pub async fn close(mut self, code: u32, reason: &str) {
        let (send, connect) = (self.send.get_mut(), self.connect.get_mut());
        let _ = tokio::time::timeout(CLOSE_DRAIN, async {
            if connect.closed.is_none() {
                send.send_data(capsule::close(code, reason).into()).await?;
            }
            send.finish().await?;
            while connect.closed.is_none() && connect.recv.data().await?.is_some() {}
            Ok::<_, Error>(())
        })
        .await;
        connect.recv.stop(Code::WT_SESSION_GONE);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.state().sessions.unregister(self.id);
        // Streams the application never took belong to a session that is gone.
        let streams = self.streams.get_mut();
        streams.close();
        while let Ok(mut stream) = streams.try_recv() {
            stream.stop(Code::WT_SESSION_GONE);
        }
    }
}

/// Waits up to 5 s for the peer's SETTINGS, then applies the WebTransport rules to them.
async fn dialect(shared: &Shared) -> Option<Dialect> {
    let mut peer = shared.peer.subscribe();
    let peer = *tokio::time::timeout(SETTINGS_WAIT, peer.wait_for(Option::is_some))
        .await
        .ok()?
        .ok()?;
    peer?.webtransport(shared.quic.max_datagram_size().is_some())
}
