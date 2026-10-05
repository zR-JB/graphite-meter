//! HTTP/2 through the h2 fork: each connection's floor and settings, its streams within their exchange bounds, the
//! stream replies are pumped into, and the receive window an admitted upload funds.

mod window;

use super::{
    body::{Aborted, ReplyBound, Sink, pump},
    lifecycle::{Event, Grace, Lifecycle},
    tls,
    window::Incoming,
};
use crate::{
    app::{App, Connection, Endpoint, MAX_HEAD_BYTES, Outcome},
    exchange::Exchange,
    lane::Work,
};
use bytes::Bytes;
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_proto::lane::IDLE_BOUND;
use h2::{
    Reason, RecvStream, SendStream,
    server::{Builder, SendResponse},
};
use http::{Method, Request, Response, response::Parts};
use std::{
    future::{Future, poll_fn},
    io,
    net::SocketAddr,
    pin::{Pin, pin},
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    time::{Sleep, sleep, timeout},
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use window::Window;

/// What a connection holds of the buffer budget from accept until its task ends.
pub const FLOOR_BYTES: usize = TRANSPORT_BYTES + STATE_BYTES;
/// TLS records and deframer, frame reads, the write buffer, HPACK and the default window.
const TRANSPORT_BYTES: usize = 512 << 10;
/// Decoded headers, buffered DATA and queued response metadata, which h2 charges to the floor as they fill.
const STATE_BYTES: usize = 1 << 20;
/// Go's stream receive window; the connection's opens to 16 MiB for admitted uploads.
const STREAM_WINDOW: u32 = 8 << 20;
const MAX_STREAMS: u32 = 250;
/// 64 KiB frames cut the CPU cost per byte of bulk transfers by about a quarter.
const FRAME_BYTES: usize = 64 << 10;
/// The client preface and first SETTINGS arrive within this.
const PREFACE_BOUND: Duration = Duration::from_secs(10);
/// Go's TCP_NOTSENT_LOWAT: unsent downloads wait in h2's scheduler, where control replies interleave.
#[cfg(target_os = "linux")]
const NOTSENT_LOWAT: u32 = 64 << 10;

/// What an HTTP/2 listener's connections share.
#[derive(Clone)]
pub struct Http2 {
    pub app: Arc<App>,
    pub tls: TlsAcceptor,
    pub shutdown: CancellationToken,
}

impl Http2 {
    /// An accepted connection's work, holding its floor from now; a connection whose floor does not fit closes.
    pub fn connection(&self, socket: TcpStream, peer: SocketAddr) -> impl Future<Output = ()> + Send + use<> {
        let floor = self.app.budget().lease(FLOOR_BYTES);
        let _ = socket.set_nodelay(true);
        #[cfg(target_os = "linux")]
        let _ = socket2::SockRef::from(&socket).set_tcp_notsent_lowat(NOTSENT_LOWAT);
        let (listener, socket) = (self.clone(), socket.into_std());
        async move {
            let (Some(_floor), Ok(socket)) = (floor, socket.and_then(TcpStream::from_std)) else {
                return;
            };
            let stream = tokio::select! {
                biased;
                () = listener.shutdown.cancelled() => return,
                stream = tls::accept(&listener.tls, socket, peer) => stream,
            };
            let Some(stream) = stream.filter(|stream| stream.get_ref().1.alpn_protocol() == Some(b"h2")) else {
                return;
            };
            let connection = Connection {
                endpoint: Endpoint::H2,
                peer: peer.ip(),
                work: Work::default(),
            };
            listener
                .serve(Stalled { inner: stream, stalled: None }, connection)
                .await;
        }
    }

    /// Serves streams until the connection ends or its lifecycle closes it.
    async fn serve<S>(&self, stream: S, connection: Connection)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut builder = Builder::new();
        builder
            .initial_window_size(STREAM_WINDOW)
            .max_frame_size(FRAME_BYTES as u32)
            .max_header_list_size(MAX_HEAD_BYTES as u32)
            .max_concurrent_streams(MAX_STREAMS)
            .max_send_buffer_size(4 * FRAME_BYTES)
            .data_frame_budget(STATE_BYTES)
            .shared_budget(self.app.budget().h2(), STATE_BYTES);
        let Ok(Ok(mut h2)) = timeout(PREFACE_BOUND, builder.handshake::<_, Bytes>(stream)).await else {
            return;
        };
        let window = Arc::new(Window::new(self.app.clone()));
        let mut lifecycle = Lifecycle::new(connection.work.clone(), Grace::Fresh);
        let mut streams = FuturesUnordered::new();
        let mut stopping = pin!(self.shutdown.cancelled());
        poll_fn(|cx| {
            if stopping.as_mut().poll(cx).is_ready() {
                lifecycle.stop();
            }
            // Streams first, on a fresh coop budget: socket reads spend it, and a spent budget stalls their selects.
            while let Poll::Ready(Some(())) = streams.poll_next_unpin(cx) {}
            loop {
                match h2.poll_accept(cx) {
                    Poll::Ready(Some(Ok((request, respond)))) if streams.len() < MAX_STREAMS as usize => {
                        streams.push(self.stream(request, respond, &connection, &window));
                        cx.waker().wake_by_ref();
                    }
                    Poll::Ready(Some(Ok((_, mut respond)))) => respond.send_reset(Reason::REFUSED_STREAM),
                    Poll::Ready(Some(Err(_)) | None) => return Poll::Ready(()),
                    Poll::Pending => break,
                }
            }
            // Queued END_STREAM and reset frames keep h2's streams after their futures end.
            let live = !streams.is_empty() || h2.has_streams();
            while let Poll::Ready(event) = lifecycle.poll(cx, window.raised(), live) {
                match event {
                    Event::GoAway => {
                        h2.graceful_shutdown();
                        cx.waker().wake_by_ref();
                    }
                    Event::Close => return Poll::Ready(()),
                }
            }
            Poll::Pending
        })
        .await;
    }

    /// One stream: reset when its exchange expires unadmitted or its reply ends unwritten, and counted as admitted
    /// work from the poll that sees it admitted until it ends.
    fn stream(
        &self,
        request: Request<RecvStream>,
        mut respond: SendResponse<Bytes>,
        connection: &Connection,
        window: &Arc<Window>,
    ) -> impl Future<Output = ()> + Send + use<> {
        let (app, connection, window) = (self.app.clone(), connection.clone(), window.clone());
        async move {
            let exchange = Exchange::start();
            let watch = exchange.watch();
            let head = request.method() == Method::HEAD;
            let request = request.map(|stream| Incoming::new(stream, watch.clone(), window));
            let served = async {
                match app.handle(request, &connection, exchange).await {
                    Outcome::Response(response) => {
                        let mut reply = Reply { respond: &mut respond, stream: None };
                        pump(&mut reply, response, head).await
                    }
                    Outcome::WebSocket(..) | Outcome::WebTransport(..) | Outcome::Abort => Err(Aborted),
                }
            };
            if watch.bounded(served, &connection.work).await != Some(Ok(())) {
                respond.send_reset(Reason::CANCEL);
            }
        }
    }
}

/// An HTTP/2 stream's reply: its head ends the stream when nothing follows, data goes in frames flow control admits.
struct Reply<'a> {
    respond: &'a mut SendResponse<Bytes>,
    stream: Option<SendStream<Bytes>>,
}

impl Reply<'_> {
    fn stream(&mut self) -> Result<&mut SendStream<Bytes>, Aborted> {
        self.stream.as_mut().ok_or(Aborted)
    }
}

impl Sink for Reply<'_> {
    async fn head(&mut self, head: Parts, end: bool, _: &mut ReplyBound) -> Result<(), Aborted> {
        let stream = self.respond.send_response(Response::from_parts(head, ()), end);
        self.stream = Some(stream.map_err(|_| Aborted)?);
        Ok(())
    }

    async fn data(&mut self, mut data: Bytes, last: bool, bound: &mut ReplyBound) -> Result<(), Aborted> {
        let stream = self.stream()?;
        while !data.is_empty() {
            let length = data.len().min(FRAME_BYTES);
            let capacity = capacity(stream, bound, length).await?;
            let chunk = data.split_to(length.min(capacity));
            stream.send_data(chunk, last && data.is_empty()).map_err(|_| Aborted)?;
            bound.progressed();
        }
        Ok(())
    }

    async fn end(&mut self, _: &mut ReplyBound) -> Result<(), Aborted> {
        self.stream()?.send_data(Bytes::new(), true).map_err(|_| Aborted)
    }

    fn poll_reset(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match &mut self.stream {
            Some(stream) => stream.poll_reset(cx).map(drop),
            None => Poll::Pending,
        }
    }
}

/// Up to `length` bytes the stream may send, within peer flow control and h2's send buffer.
async fn capacity(stream: &mut SendStream<Bytes>, bound: &mut ReplyBound, length: usize) -> Result<usize, Aborted> {
    stream.reserve_capacity(length);
    poll_fn(|cx| {
        loop {
            bound.check(cx)?;
            if stream.capacity() > 0 {
                return Poll::Ready(Ok(stream.capacity()));
            }
            match stream.poll_capacity(cx) {
                Poll::Ready(Some(Ok(_))) => {}
                Poll::Ready(Some(Err(_)) | None) => return Poll::Ready(Err(Aborted)),
                Poll::Pending => {
                    bound.blocked(cx)?;
                    return Poll::Pending;
                }
            }
        }
    })
    .await
}

/// A connection's socket, ended by a write blocked for the idle bound: a blocked socket stalls every stream.
struct Stalled<S> {
    inner: S,
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<S> Stalled<S> {
    /// A write is blocked; fails once it has been for the idle bound.
    fn blocked(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let stalled = self.stalled.get_or_insert_with(|| Box::pin(sleep(IDLE_BOUND)));
        match stalled.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(io::ErrorKind::TimedOut.into())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn written(&mut self, cx: &mut Context<'_>, result: Poll<io::Result<usize>>) -> Poll<io::Result<usize>> {
        match result {
            Poll::Ready(Ok(1..)) => {
                self.stalled = None;
                result
            }
            Poll::Ready(_) => result,
            Poll::Pending => self.blocked(cx).map_ok(|()| 0),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Stalled<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if self
            .stalled
            .as_mut()
            .is_some_and(|stalled| stalled.as_mut().poll(cx).is_ready())
        {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Stalled<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, bytes);
        self.written(cx, result)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write_vectored(cx, bytes);
        self.written(cx, result)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                self.stalled = None;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => self.blocked(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_shutdown(cx) {
            Poll::Pending => self.blocked(cx),
            result => result,
        }
    }
}
