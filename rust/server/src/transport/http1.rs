//! HTTP/1.1 through hyper: an IO wrapper closing when exchange, reply or lane calls for it; WebSocket hand-off.

use super::{
    Burst, Feed, Rehome,
    body::{Aborted, Body, ReplyBound},
    tls,
    websocket::{self, Socket},
};
use crate::{
    app::{App, Connection, Endpoint, Outcome},
    exchange::{EXCHANGE_BOUND, Exchange, Watch},
    lock,
};
use bytes::Bytes;
use futures_util::task::AtomicWaker;
use graphite_meter_http3::CONNECTION_IDLE;
use http::{Method, Request, Response};
use http_body::{Frame, SizeHint};
use hyper::{
    body::Incoming,
    server::conn::http1,
    service::service_fn,
    upgrade::{OnUpgrade, Parts, Upgraded},
};
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
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    runtime::Handle,
    time::{Instant, Sleep, sleep_until},
};
use tokio_rustls::TlsAcceptor;

/// hyper's buffer: 128 KiB reads cost less CPU per uploaded byte than 64 KiB; it also bounds a head before parsing.
const BUFFER_BYTES: usize = 128 << 10;

#[derive(Clone)]
pub struct Http1 {
    pub app: Arc<App>,
    pub endpoint: Endpoint,
    pub tls: Option<TlsAcceptor>,
    /// The runtime WebSocket buses move to, which runs no bulk transfer.
    pub buses: Handle,
}

type Bus = Arc<Mutex<Option<(OnUpgrade, Socket)>>>;

impl Http1 {
    /// An accepted connection's work, after its TLS handshake on a TLS listener, on whichever runtime polls it.
    pub fn connection(
        &self,
        socket: TcpStream,
        peer: SocketAddr,
    ) -> impl Future<Output = ()> + Send + use<> {
        let (listener, socket, accepted) = (self.clone(), socket.into_std(), Instant::now());
        async move {
            let Ok(socket) = socket.and_then(TcpStream::from_std) else {
                return;
            };
            let _ = socket.set_nodelay(true);
            let connection = Connection::new(listener.endpoint, peer.ip());
            let Some(acceptor) = &listener.tls else {
                return listener.serve(socket, connection, accepted).await;
            };
            let stream = tokio::select! {
                biased;
                () = listener.app.shutdown().cancelled() => return,
                stream = tls::accept(acceptor, socket, peer) => stream,
            };
            if let Some(stream) = stream {
                listener.serve(stream, connection, Instant::now()).await;
            }
        }
    }

    /// Serves requests until the connection ends; the first request's exchange began at `start`.
    async fn serve<S>(&self, stream: S, connection: Connection, start: Instant)
    where
        S: AsyncRead + AsyncWrite + Rehome + Unpin + Send + 'static,
    {
        let shared = Arc::new(Shared::default());
        let io = Io::new(stream, shared.clone(), start + EXCHANGE_BOUND);
        let bus = Bus::default();
        let service = {
            let (app, bus) = (self.app.clone(), bus.clone());
            service_fn(move |request| {
                respond(
                    app.clone(),
                    request,
                    connection.clone(),
                    shared.clone(),
                    bus.clone(),
                )
            })
        };
        let mut serving = pin!(
            http1::Builder::new()
                .max_buf_size(BUFFER_BYTES)
                .header_read_timeout(None)
                .serve_connection(TokioIo::new(io), service)
                .with_upgrades()
        );
        // A child token: polling it never takes the server-wide token's waiter lock.
        let shutdown = self.app.shutdown().child_token();
        tokio::select! {
            biased;
            _ = serving.as_mut() => {}
            () = shutdown.cancelled() => {
                serving.as_mut().graceful_shutdown();
                let _ = serving.await;
            }
        }
        let upgraded = lock(&bus).take();
        if let Some((upgrade, socket)) = upgraded {
            // This task, holding the connection's admission, waits off the socket's path.
            let _ = self.buses.spawn(run_bus::<S>(upgrade, socket)).await;
        }
    }
}

/// Runs a WebSocket on the calling runtime, its socket moved there, until its lane ends or the peer leaves.
async fn run_bus<S>(upgrade: OnUpgrade, socket: Socket)
where
    S: AsyncRead + AsyncWrite + Rehome + Unpin + Send + 'static,
{
    let upgraded = match socket.lane() {
        Some(lane) => tokio::select! {
            biased;
            _ = lane.ended() => return,
            upgraded = upgrade => upgraded,
        },
        None => upgrade.await,
    };
    let Ok(Ok(Parts { io, read_buf, .. })) = upgraded.map(Upgraded::downcast::<TokioIo<Io<S>>>)
    else {
        return;
    };
    let mut stream = io.into_inner().inner;
    // A socket that cannot move still works through its first runtime's driver.
    let _ = stream.rehome();
    match socket {
        Socket::Feed(..) => websocket::serve(Feed::new(stream), read_buf, socket).await,
        _ => websocket::serve(stream, read_buf, socket).await,
    }
}

/// Answers one request; an error closes the connection unanswered.
async fn respond(
    app: Arc<App>,
    mut request: Request<Incoming>,
    connection: Connection,
    shared: Arc<Shared>,
    bus: Bus,
) -> io::Result<Response<H1Reply>> {
    let head = request.method() == Method::HEAD;
    let upgrade = hyper::upgrade::on(&mut request);
    let exchange = shared.exchange();
    match app.handle(request, &connection, exchange).await {
        Outcome::Response(response) => Ok(shared.reply(response, head)),
        Outcome::WebSocket(response, socket) => {
            *lock(&bus) = Some((upgrade, socket));
            shared.post(State::Open);
            Ok(response.map(|body| H1Reply { body, shared }))
        }
        Outcome::WebTransport(..) | Outcome::Abort => Err(io::ErrorKind::ConnectionAborted.into()),
    }
}

#[derive(Default)]
struct Shared {
    posted: Mutex<Option<State>>,
    changed: AtomicBool,
    /// The deadline of the exchange a request's first byte started.
    started: Mutex<Option<Instant>>,
    /// The reply's body ended, so the flush after it delivers the reply.
    complete: AtomicBool,
    unflushed: AtomicBool,
    flushed: AtomicWaker,
    feed: AtomicBool,
}

impl Shared {
    fn post(&self, state: State) {
        *lock(&self.posted) = Some(state);
        self.changed.store(true, Ordering::Release);
    }

    /// The exchange of a request whose head was just parsed, which bounds the connection until admitted.
    fn exchange(&self) -> Exchange {
        let started = lock(&self.started).take();
        let exchange = Exchange::until(started.unwrap_or_else(|| Instant::now() + EXCHANGE_BOUND));
        self.post(State::Exchange(Some(exchange.watch())));
        exchange
    }

    /// Bounds the connection by the reply's bound; a reply without a body to send is complete at once.
    fn reply(self: &Arc<Self>, response: Response<Body>, head: bool) -> Response<H1Reply> {
        let complete = head || http_body::Body::is_end_stream(response.body());
        self.complete.store(complete, Ordering::Release);
        self.feed
            .store(response.body().is_feed(), Ordering::Release);
        self.post(State::Reply(ReplyBound::of(response.body())));
        response.map(|body| H1Reply {
            body,
            shared: self.clone(),
        })
    }
}

struct H1Reply {
    body: Body,
    shared: Arc<Shared>,
}

impl http_body::Body for H1Reply {
    type Data = Bytes;
    type Error = Aborted;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Aborted>>> {
        let this = self.get_mut();
        // A feed takes its next line only once the last left hyper's buffer, so a stall queues no stale lines.
        let feed = this.body.is_feed();
        if feed && this.shared.unflushed.load(Ordering::Acquire) {
            this.shared.flushed.register(cx.waker());
            if this.shared.unflushed.load(Ordering::Acquire) {
                return Poll::Pending;
            }
        }
        let frame = ready!(Pin::new(&mut this.body).poll_frame(cx));
        if feed && matches!(frame, Some(Ok(_))) {
            this.shared.unflushed.store(true, Ordering::Release);
        }
        if frame.is_none() || this.body.is_end_stream() {
            this.shared.complete.store(true, Ordering::Release);
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

struct Io<S> {
    inner: S,
    state: State,
    deadline: Pin<Box<Sleep>>,
    shared: Arc<Shared>,
    burst: Burst,
    feed: bool,
}

enum State {
    /// Between requests until the deadline; a first byte starts an exchange.
    Waiting,
    /// A request until admitted or answered by the deadline; its watch once its head is parsed.
    Exchange(Option<Watch>),
    Reply(ReplyBound),
    /// Admitted work reading its request, or an upgraded connection.
    Open,
}

impl<S> Io<S> {
    fn new(inner: S, shared: Arc<Shared>, deadline: Instant) -> Self {
        *lock(&shared.started) = Some(deadline);
        let (state, deadline) = (State::Exchange(None), Box::pin(sleep_until(deadline)));
        Self {
            inner,
            state,
            deadline,
            shared,
            burst: Burst::default(),
            feed: false,
        }
    }

    /// Adopts a posted limit and fails once the connection's deadline passed or its reply must end.
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.shared.changed.load(Ordering::Acquire)
            && self.shared.changed.swap(false, Ordering::Acquire)
        {
            self.adopt();
        }
        match &mut self.state {
            State::Reply(bound) => Ok(bound.check(cx)?),
            State::Open => Ok(()),
            State::Waiting | State::Exchange(_) if self.deadline.as_mut().poll(cx).is_pending() => {
                Ok(())
            }
            // A request admitted while its body is read is bound by its lane instead.
            State::Exchange(Some(watch)) if watch.admitted().is_some() => {
                self.state = State::Open;
                Ok(())
            }
            State::Waiting | State::Exchange(_) => Err(io::ErrorKind::TimedOut.into()),
        }
    }

    fn adopt(&mut self) {
        let Some(state) = lock(&self.shared.posted).take() else {
            return;
        };
        if let State::Exchange(Some(watch)) = &state {
            self.deadline.as_mut().reset(watch.deadline());
        }
        self.state = state;
    }

    fn blocked(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if let State::Reply(bound) = &mut self.state {
            bound.blocked(cx)?;
        }
        Ok(())
    }

    fn written(
        &mut self,
        cx: &mut Context<'_>,
        result: Poll<io::Result<usize>>,
    ) -> Poll<io::Result<usize>> {
        match &result {
            Poll::Ready(Ok(written @ 1..)) => {
                self.burst.moved(*written);
                if let State::Reply(bound) = &self.state {
                    bound.progressed();
                }
            }
            Poll::Pending => {
                self.burst.waited();
                self.blocked(cx)?;
            }
            Poll::Ready(_) => {}
        }
        result
    }

    /// The reply reached the socket, and the connection waits for the next request.
    fn delivered(&mut self) -> io::Result<()> {
        let State::Reply(bound) = &self.state else {
            return Ok(());
        };
        bound.delivered()?;
        self.deadline
            .as_mut()
            .reset(Instant::now() + CONNECTION_IDLE);
        self.state = State::Waiting;
        Ok(())
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Io<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        this.check(cx)?;
        ready!(this.burst.poll_turn(cx));
        let filled = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        this.burst.moved(buffer.filled().len() - filled);
        match result {
            Poll::Pending => {
                this.burst.waited();
                this.blocked(cx)?;
            }
            Poll::Ready(Ok(()))
                if buffer.filled().len() > filled && matches!(this.state, State::Waiting) =>
            {
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

impl<S: Rehome> Io<S> {
    fn match_feed(&mut self) {
        let feed = self.shared.feed.load(Ordering::Acquire);
        if feed != self.feed {
            self.feed = feed;
            Feed::bound(&mut self.inner, feed);
        }
    }
}

impl<S: AsyncWrite + Rehome + Unpin> AsyncWrite for Io<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        this.check(cx)?;
        this.match_feed();
        ready!(this.burst.poll_turn(cx));
        let result = match this.feed && Feed::<S>::RECORDS {
            true => Feed::poll_send(&mut this.inner, cx, &[io::IoSlice::new(bytes)]),
            false => Pin::new(&mut this.inner).poll_write(cx, bytes),
        };
        this.written(cx, result)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        this.check(cx)?;
        this.match_feed();
        ready!(this.burst.poll_turn(cx));
        let result = match this.feed && Feed::<S>::RECORDS {
            true => Feed::poll_send(&mut this.inner, cx, bytes),
            false => Pin::new(&mut this.inner).poll_write_vectored(cx, bytes),
        };
        this.written(cx, result)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        this.check(cx)?;
        let flushed = Pin::new(&mut this.inner).poll_flush(cx);
        if flushed.is_ready() && this.shared.unflushed.swap(false, Ordering::AcqRel) {
            this.shared.flushed.wake();
        }
        match flushed {
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
        let this = &mut *self;
        this.check(cx)?;
        let result = Pin::new(&mut this.inner).poll_shutdown(cx);
        if result.is_pending() {
            this.blocked(cx)?;
        }
        result
    }
}
