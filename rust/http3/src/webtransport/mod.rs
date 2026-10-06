//! WebTransport over HTTP/3 in either dialect, one session per connection: streams, datagrams, resets, close sequence.
mod connect;
mod registry;
mod stream;

pub use stream::{RecvStream, SendStream};
pub(crate) use {
    connect::{Connect, Sessions},
    registry::{Registry, Unrouted},
};

use crate::{
    budget::Charge, capsule, client::SendRequest, code::Code, driver::Shared, error::Error, settings::Dialect,
    stream::RequestStream,
};
use bytes::Bytes;
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::{Mutex, mpsc, watch};

/// How long a server waits for its client's SETTINGS, as webtransport-go's does.
const SETTINGS_WAIT: Duration = Duration::from_secs(5);
/// How long the refusal of a CONNECT without a dialect may take to write.
const REFUSAL_TIMEOUT: Duration = Duration::from_secs(10);
/// State a session keeps apart from its CONNECT stream: queue slots, the capsule reader and both sides' close reasons.
const SESSION_BYTES: usize = size_of::<Session>()
    + size_of::<Connect>()
    + registry::STREAM_QUEUE * size_of::<RecvStream>()
    + registry::DATAGRAM_QUEUE * size_of::<Bytes>()
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
