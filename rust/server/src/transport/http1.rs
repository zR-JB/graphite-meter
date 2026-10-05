//! HTTP/1.1 through hyper: an IO wrapper that closes the connection when its exchange, reply or lane calls for it,
//! and the hand-off of WebSocket upgrades.

use super::{
    body::{Body, Bound},
    tls, websocket,
};
use crate::{
    app::{App, Connection, Endpoint, Outcome},
    lane::{EXCHANGE_BOUND, Exchange, Lane, Watch, Work},
    lock,
};
use bytes::Bytes;
use graphite_meter_proto::lane::LaneEnding;
use http::{Method, Request, Response};
use http_body::{Frame, SizeHint};
use hyper::{body::Incoming, server::conn::http1, service::service_fn, upgrade::OnUpgrade};
use hyper_util::rt::TokioIo;
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    time::{Instant, Sleep, sleep_until},
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

/// hyper's buffer: 128 KiB reads cost less CPU per uploaded byte than 64 KiB; it also bounds a head before parsing.
const BUFFER_BYTES: usize = 128 << 10;
/// A connection between requests closes after this long.
const KEEP_ALIVE_IDLE: Duration = Duration::from_secs(15);

/// What an HTTP/1 listener's connections share.
#[derive(Clone)]
pub struct Http1 {
    pub app: Arc<App>,
    pub endpoint: Endpoint,
    /// Present on a TLS listener.
    pub tls: Option<TlsAcceptor>,
    pub shutdown: CancellationToken,
}

type Bus = Arc<Mutex<Option<(OnUpgrade, Lane)>>>;

impl Http1 {
    /// An accepted connection's work, after its TLS handshake on a TLS listener, on whichever runtime polls it.
    pub fn connection(&self, socket: TcpStream, peer: SocketAddr) -> impl Future<Output = ()> + Send + 'static {
        let (listener, socket) = (self.clone(), socket.into_std());
        async move {
            let Ok(socket) = socket.and_then(TcpStream::from_std) else {
                return;
            };
            let _ = socket.set_nodelay(true);
            let connection = Connection {
                endpoint: listener.endpoint,
                peer: peer.ip(),
                work: Work::default(),
            };
            let Some(acceptor) = &listener.tls else {
                return listener.serve(socket, connection).await;
            };
            let stream = tokio::select! {
                biased;
                () = listener.shutdown.cancelled() => return,
                stream = tls::accept(acceptor, socket, peer) => stream,
            };
            if let Some(stream) = stream {
                listener.serve(stream, connection).await;
            }
        }
    }

    async fn serve<S>(&self, stream: S, connection: Connection)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let shared = Arc::new(Shared::default());
        let io = Io::new(stream, shared.clone());
        let bus = Bus::default();
        let service = {
            let (app, bus) = (self.app.clone(), bus.clone());
            service_fn(move |request| respond(app.clone(), request, connection.clone(), shared.clone(), bus.clone()))
        };
        let mut serving = pin!(
            http1::Builder::new()
                .max_buf_size(BUFFER_BYTES)
                .header_read_timeout(None)
                .serve_connection(TokioIo::new(io), service)
                .with_upgrades()
        );
        tokio::select! {
            biased;
            _ = serving.as_mut() => {}
            () = self.shutdown.cancelled() => {
                serving.as_mut().graceful_shutdown();
                let _ = serving.await;
            }
        }
        let upgraded = lock(&bus).take();
        if let Some((upgrade, lane)) = upgraded {
            websocket::serve(upgrade, lane).await;
        }
    }
}

/// Answers one request; an error closes the connection unanswered.
async fn respond(
    app: Arc<App>,
    mut request: Request<Incoming>,
    connection: Connection,
    shared: Arc<Shared>,
    bus: Bus,
) -> io::Result<Response<Reply>> {
    let head = request.method() == Method::HEAD;
    let upgrade = hyper::upgrade::on(&mut request);
    let exchange = shared.exchange();
    match app.handle(request, &connection, exchange).await {
        Outcome::Response(response) => Ok(shared.reply(response, head)),
        Outcome::WebSocket(response, lane) => {
            *lock(&bus) = Some((upgrade, lane));
            shared.post(Limit::Open);
            Ok(response.map(|body| Reply { body, shared }))
        }
        Outcome::Abort => Err(io::ErrorKind::ConnectionAborted.into()),
    }
}

/// What bounds the connection: posted by the service, enforced by the IO wrapper.
enum Limit {
    /// A parsed request until admitted.
    Exchange(Watch),
    Until(Instant),
    Lane(Lane),
    Open,
}

/// What the service and the IO wrapper of one connection share.
#[derive(Default)]
struct Shared {
    posted: Mutex<Option<Limit>>,
    changed: AtomicBool,
    /// The deadline of the exchange a request's first byte started.
    started: Mutex<Option<Instant>>,
    /// The reply's body ended, so the flush after it ends the exchange.
    complete: AtomicBool,
}

impl Shared {
    fn post(&self, limit: Limit) {
        *lock(&self.posted) = Some(limit);
        self.changed.store(true, Ordering::Release);
    }

    /// The exchange of a request whose head was just parsed, which bounds the connection until admitted.
    fn exchange(&self) -> Exchange {
        let started = lock(&self.started).take();
        let exchange = Exchange::until(started.unwrap_or_else(|| Instant::now() + EXCHANGE_BOUND));
        self.post(Limit::Exchange(exchange.watch()));
        exchange
    }

    /// Bounds the connection by the reply's bound; a reply without a body to send is complete at once.
    fn reply(self: &Arc<Self>, response: Response<Body>, head: bool) -> Response<Reply> {
        let limit = match response.body().bound() {
            Some(Bound::Lane(lane)) => Limit::Lane(lane.clone()),
            Some(Bound::Until(deadline)) => Limit::Until(*deadline),
            None => Limit::Until(Instant::now() + EXCHANGE_BOUND),
        };
        let complete = head || http_body::Body::is_end_stream(response.body());
        self.complete.store(complete, Ordering::Release);
        self.post(limit);
        response.map(|body| Reply { body, shared: self.clone() })
    }
}

/// A reply as hyper writes it: its lane's other endings abort it, and its end completes the exchange.
struct Reply {
    body: Body,
    shared: Arc<Shared>,
}

impl http_body::Body for Reply {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if let Some(Bound::Lane(lane)) = this.body.bound()
            && lane.due().is_some_and(|ending| ending != LaneEnding::Finished)
        {
            return Poll::Ready(Some(Err(ended())));
        }
        let frame = ready!(Pin::new(&mut this.body).poll_frame(cx));
        if frame.is_none() || this.body.is_end_stream() {
            this.shared.complete.store(true, Ordering::Release);
        }
        Poll::Ready(frame.map(|frame| frame.map_err(|never| match never {})))
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

fn ended() -> io::Error {
    io::ErrorKind::ConnectionAborted.into()
}

/// The socket as hyper sees it.
struct Io<S> {
    inner: S,
    state: State,
    /// The keep-alive or exchange deadline.
    deadline: Pin<Box<Sleep>>,
    shared: Arc<Shared>,
}

enum State {
    /// Between requests until the deadline; a first byte starts an exchange.
    Waiting,
    /// A request until admitted or answered by the deadline; its watch once its head is parsed.
    Exchange(Option<Watch>),
    /// A reply until written by the deadline.
    Until,
    /// A reply its lane bounds, and the lane's ending, polled while the socket blocks.
    Lane(Lane, Pin<Box<dyn Future<Output = LaneEnding> + Send>>),
    /// Admitted work without a reply yet, or an upgraded connection.
    Open,
}

impl<S> Io<S> {
    fn new(inner: S, shared: Arc<Shared>) -> Self {
        let deadline = Box::pin(sleep_until(Instant::now() + KEEP_ALIVE_IDLE));
        Self { inner, state: State::Waiting, deadline, shared }
    }

    /// Adopts a posted limit and fails once the connection's deadline passed or its lane ended otherwise.
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.shared.changed.load(Ordering::Acquire) && self.shared.changed.swap(false, Ordering::Acquire) {
            self.adopt();
        }
        let expired = match &self.state {
            State::Waiting | State::Exchange(_) | State::Until => self.deadline.as_mut().poll(cx).is_ready(),
            State::Lane(lane, _) if lane.ending().is_some_and(|ending| ending != LaneEnding::Finished) => {
                return Err(ended());
            }
            State::Lane(..) | State::Open => false,
        };
        match &self.state {
            _ if !expired => {}
            // A request admitted while its body is read is bound by its lane instead.
            State::Exchange(Some(watch)) if watch.admitted().is_some() => self.state = State::Open,
            _ => return Err(io::ErrorKind::TimedOut.into()),
        }
        Ok(())
    }

    fn adopt(&mut self) {
        let Some(limit) = lock(&self.shared.posted).take() else {
            return;
        };
        self.state = match limit {
            Limit::Exchange(watch) => {
                self.deadline.as_mut().reset(watch.deadline());
                State::Exchange(Some(watch))
            }
            Limit::Until(deadline) => {
                self.deadline.as_mut().reset(deadline);
                State::Until
            }
            Limit::Lane(lane) => {
                let ending = lane.clone();
                State::Lane(lane, Box::pin(async move { ending.ended().await }))
            }
            Limit::Open => State::Open,
        };
    }

    /// Waits for the lane's ending too while the socket blocks.
    fn blocked(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if let State::Lane(_, ending) = &mut self.state
            && let Poll::Ready(ending) = ending.as_mut().poll(cx)
        {
            self.state = State::Open;
            if ending != LaneEnding::Finished {
                return Err(ended());
            }
        }
        Ok(())
    }

    fn written(&mut self, cx: &mut Context<'_>, result: Poll<io::Result<usize>>) -> Poll<io::Result<usize>> {
        match &result {
            Poll::Ready(Ok(1..)) => {
                if let State::Lane(lane, _) = &self.state {
                    lane.moved();
                }
            }
            Poll::Pending => self.blocked(cx)?,
            Poll::Ready(_) => {}
        }
        result
    }

    /// The reply reached the socket: its lane finishes, and the connection waits for the next request.
    fn delivered(&mut self) -> io::Result<()> {
        if let State::Lane(lane, _) = &self.state
            && lane.finish() != LaneEnding::Finished
        {
            return Err(ended());
        }
        self.deadline.as_mut().reset(Instant::now() + KEEP_ALIVE_IDLE);
        self.state = State::Waiting;
        Ok(())
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Io<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        this.check(cx)?;
        let filled = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        match result {
            Poll::Pending => this.blocked(cx)?,
            Poll::Ready(Ok(())) if buffer.filled().len() > filled && matches!(this.state, State::Waiting) => {
                let deadline = Instant::now() + EXCHANGE_BOUND;
                this.deadline.as_mut().reset(deadline);
                this.state = State::Exchange(None);
                *lock(&this.shared.started) = Some(deadline);
            }
            Poll::Ready(_) => {}
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Io<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        this.check(cx)?;
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        this.written(cx, result)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        this.check(cx)?;
        let result = Pin::new(&mut this.inner).poll_write_vectored(cx, bytes);
        this.written(cx, result)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        this.check(cx)?;
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Pending => {
                this.blocked(cx)?;
                Poll::Pending
            }
            Poll::Ready(Ok(())) if this.shared.complete.load(Ordering::Acquire) => {
                this.shared.complete.store(false, Ordering::Release);
                Poll::Ready(this.delivered())
            }
            result => result,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
