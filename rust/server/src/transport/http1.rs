//! HTTP/1.1 through hyper: an IO wrapper closing when exchange, reply or lane calls for it; WebSocket hand-off.

#[cfg(target_os = "linux")]
use super::lowat::{BulkRead, UPLOAD_READ_MIN};
use super::{
    Burst, Feed, Rehome,
    body::{Aborted, Body, ReplyBound},
    lifecycle::CONNECTION_IDLE,
    tls,
    websocket::{self, Socket},
};
#[cfg(target_os = "linux")]
use crate::engine::download::Sendfile;
use crate::{
    app::{App, Connection, Endpoint, Outcome},
    exchange::{EXCHANGE_BOUND, Exchange, Watch},
    lock,
};
use bytes::Bytes;
use futures_util::task::AtomicWaker;
use graphite_meter_proto::route::Route;
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
    task::JoinSet,
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
            super::bound_unsent(&socket, super::NOTSENT_LOWAT);
            let connection = Connection::new(listener.endpoint, peer.ip());
            let Some(acceptor) = &listener.tls else {
                #[cfg(target_os = "linux")]
                if !connection.endpoint.bootstrap() {
                    return listener.serve_plain(socket, connection, accepted).await;
                }
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
        let io = Io::new(
            stream,
            shared.clone(),
            start + EXCHANGE_BOUND,
            #[cfg(target_os = "linux")]
            self.app.download_sendfile(),
        );
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
            // This task, holding the connection's admission, waits off the socket's path; the drain's abort ends both.
            let mut bus = JoinSet::new();
            bus.spawn_on(run_bus::<S>(upgrade, socket), &self.buses);
            bus.join_next().await;
        }
    }
}

#[cfg(target_os = "linux")]
struct Prefixed {
    prefix: Vec<u8>,
    offset: usize,
    inner: TcpStream,
}

#[cfg(target_os = "linux")]
impl Prefixed {
    fn new(prefix: Vec<u8>, inner: TcpStream) -> Self {
        Self {
            prefix,
            offset: 0,
            inner,
        }
    }
}

#[cfg(target_os = "linux")]
impl Rehome for Prefixed {
    const PLAIN: bool = true;

    fn socket(&mut self) -> &mut TcpStream {
        &mut self.inner
    }
}

#[cfg(target_os = "linux")]
impl AsyncRead for Prefixed {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.offset < this.prefix.len() {
            let avail = this.prefix.len() - this.offset;
            let n = avail.min(buf.remaining());
            buf.put_slice(&this.prefix[this.offset..this.offset + n]);
            this.offset += n;
            if this.offset == this.prefix.len() {
                this.prefix = Vec::new();
                this.offset = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

#[cfg(target_os = "linux")]
impl AsyncWrite for Prefixed {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(target_os = "linux")]
enum ParsedFastHead {
    Upload {
        request: Request<Body>,
        content_length: u64,
        close: bool,
        body_offset: usize,
    },
    Download {
        request: Request<Body>,
        close: bool,
        body_offset: usize,
    },
}

#[cfg(target_os = "linux")]
fn parse_fast_head(buf: &[u8], split: usize) -> Option<ParsedFastHead> {
    use http::{HeaderName, HeaderValue, Uri, Version, header};

    const MIN_FAST_UPLOAD: u64 = 64 << 10;
    let head = std::str::from_utf8(&buf[..split]).ok()?;
    let mut lines = head.split("\r\n");
    let first = lines.next()?;
    let (method, rest) = if let Some(rest) = first.strip_prefix("POST ") {
        (Method::POST, rest)
    } else {
        (Method::GET, first.strip_prefix("GET ")?)
    };
    let (target, version) = rest.split_once(' ')?;
    if version != "HTTP/1.1" {
        return None;
    }
    let uri: Uri = target.parse().ok()?;
    let is_upload = method == Method::POST && uri.path() == Route::Upload.path();
    let is_download = method == Method::GET && uri.path() == Route::Download.path();
    if !is_upload && !is_download {
        return None;
    }
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .version(Version::HTTP_11);
    let mut content_length: Option<u64> = None;
    let mut close = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name_raw, value_raw) = line.split_once(':')?;
        let name = HeaderName::from_bytes(name_raw.trim().as_bytes()).ok()?;
        let val_str = value_raw.trim();
        let value = HeaderValue::from_bytes(val_str.as_bytes()).ok()?;
        if name == header::TRANSFER_ENCODING || name == header::EXPECT || name == header::UPGRADE {
            return None;
        }
        if name == header::CONTENT_LENGTH {
            if content_length.is_some() {
                return None;
            }
            content_length = Some(val_str.parse::<u64>().ok()?);
        } else if name == header::CONNECTION
            && val_str
                .split(',')
                .any(|tok| tok.trim().eq_ignore_ascii_case("close"))
        {
            close = true;
        }
        builder = builder.header(name, value);
    }
    let request = builder.body(Body::empty()).ok()?;
    if is_upload {
        let content_length = content_length.filter(|&len| len >= MIN_FAST_UPLOAD)?;
        Some(ParsedFastHead::Upload {
            request,
            content_length,
            close,
            body_offset: split + 4,
        })
    } else {
        if content_length.is_some_and(|len| len > 0) {
            return None;
        }
        Some(ParsedFastHead::Download {
            request,
            close,
            body_offset: split + 4,
        })
    }
}

#[cfg(target_os = "linux")]
fn encode_h1_head(response: &Response<Body>, force_close: bool) -> Vec<u8> {
    use http::header;
    use std::io::Write;

    let status = response.status();
    let reason = status.canonical_reason().unwrap_or("");
    let mut wire = Vec::with_capacity(512);
    let _ = write!(wire, "HTTP/1.1 {} {}\r\n", status.as_u16(), reason);
    let mut has_conn = false;
    for (name, value) in response.headers() {
        if name == header::CONNECTION {
            has_conn = true;
        }
        let _ = write!(wire, "{}: ", name.as_str());
        wire.extend_from_slice(value.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    if force_close && !has_conn {
        wire.extend_from_slice(b"connection: close\r\n");
    }
    wire.extend_from_slice(b"\r\n");
    wire
}

#[cfg(target_os = "linux")]
fn encode_h1_response(response: &Response<Body>, force_close: bool) -> Vec<u8> {
    use http::header;
    use std::io::Write;

    let body = response.body().full_bytes().unwrap_or(&[]);
    let status = response.status();
    let reason = status.canonical_reason().unwrap_or("");
    let mut wire = Vec::with_capacity(512 + body.len());
    let _ = write!(wire, "HTTP/1.1 {} {}\r\n", status.as_u16(), reason);
    let mut has_cl = false;
    let mut has_conn = false;
    for (name, value) in response.headers() {
        if name == header::CONTENT_LENGTH {
            has_cl = true;
        } else if name == header::CONNECTION {
            has_conn = true;
        }
        let _ = write!(wire, "{}: ", name.as_str());
        wire.extend_from_slice(value.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    if !has_cl {
        let _ = write!(wire, "content-length: {}\r\n", body.len());
    }
    if force_close && !has_conn {
        wire.extend_from_slice(b"connection: close\r\n");
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(body);
    wire
}

#[cfg(target_os = "linux")]
impl Http1 {
    /// Zero-copy plain HTTP/1.1 upload fast-path (`MSG_TRUNC` + adaptive `SO_RCVLOWAT`); every other request hands off to hyper.
    async fn serve_plain(&self, mut socket: TcpStream, connection: Connection, start: Instant) {
        use graphite_meter_proto::lane::LaneEnding;
        use http::header;
        use rustix::net::{RecvFlags, recv};
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt, Interest},
            time::timeout,
        };

        const MAX_PEEK: usize = 8 << 10;
        const YIELD_BYTES: u64 = 1 << 20;

        let shutdown = self.app.shutdown().child_token();
        let mut buf: Vec<u8> = Vec::new();
        let mut discard: Vec<u8> = Vec::new();
        let mut first = true;

        loop {
            let wait_deadline = if first {
                start + EXCHANGE_BOUND
            } else {
                Instant::now() + CONNECTION_IDLE
            };
            let mut request_started = if first { Some(start) } else { None };
            first = false;

            let split = loop {
                if buf.len() >= 13
                    && !buf.starts_with(b"POST /upload")
                    && !buf.starts_with(b"GET /download")
                {
                    return self
                        .serve(
                            Prefixed::new(buf, socket),
                            connection,
                            request_started.unwrap_or_else(Instant::now),
                        )
                        .await;
                }
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
                if buf.len() >= MAX_PEEK {
                    return self
                        .serve(
                            Prefixed::new(buf, socket),
                            connection,
                            request_started.unwrap_or_else(Instant::now),
                        )
                        .await;
                }
                let deadline = request_started.map_or(wait_deadline, |s| s + EXCHANGE_BOUND);
                let mut chunk = [0u8; 4096];
                let read = tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    res = tokio::time::timeout_at(deadline, socket.read(&mut chunk)) => match res {
                        Ok(Ok(n)) => n,
                        _ => return,
                    },
                };
                if read == 0 {
                    return;
                }
                if request_started.is_none() {
                    request_started = Some(Instant::now());
                }
                buf.extend_from_slice(&chunk[..read]);
            };

            let req_start = request_started.unwrap_or_else(Instant::now);
            let Some(parsed) = parse_fast_head(&buf, split) else {
                return self
                    .serve(Prefixed::new(buf, socket), connection, req_start)
                    .await;
            };

            let exchange = Exchange::until(req_start + EXCHANGE_BOUND);
            match parsed {
                ParsedFastHead::Download {
                    request,
                    close,
                    body_offset,
                } => {
                    let download =
                        match self
                            .app
                            .admit_fast_download(&request, &connection, exchange)
                        {
                            Ok(Some(download)) => download,
                            Ok(None) => {
                                return self
                                    .serve(Prefixed::new(buf, socket), connection, req_start)
                                    .await;
                            }
                            Err(refusal) => {
                                let wire = encode_h1_response(&refusal, true);
                                let _ = timeout(EXCHANGE_BOUND, socket.write_all(&wire)).await;
                                return;
                            }
                        };
                    if buf.len() > body_offset {
                        buf.copy_within(body_offset.., 0);
                        buf.truncate(buf.len() - body_offset);
                    } else {
                        buf.clear();
                    }
                    let head_wire = encode_h1_head(&download.response, close);
                    let mut head_sent = 0usize;
                    let mut ended = pin!(download.lane.ended());
                    while head_sent < head_wire.len() {
                        let sent = socket.try_io(Interest::WRITABLE, || {
                            Ok(rustix::net::send(
                                &socket,
                                &head_wire[head_sent..],
                                rustix::net::SendFlags::MORE | rustix::net::SendFlags::NOSIGNAL,
                            )?)
                        });
                        match sent {
                            Ok(0) => return,
                            Ok(n) => {
                                head_sent += n;
                                download.lane.moved();
                            }
                            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                                let ready = tokio::select! {
                                    biased;
                                    _ = &mut ended => return,
                                    res = socket.writable() => res,
                                };
                                if ready.is_err() {
                                    return;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    let fd = download.sendfile.fd();
                    let block_len = download.sendfile.len() as u64;
                    let mut remaining = download.bytes;
                    let mut offset: u64 = 0;
                    let mut since_yield: u64 = 0;
                    while remaining > 0 {
                        let len = (block_len - offset).min(remaining) as usize;
                        let sent = socket.try_io(Interest::WRITABLE, || {
                            let mut pos = offset;
                            Ok(rustix::fs::sendfile(&socket, fd, Some(&mut pos), len)?)
                        });
                        match sent {
                            Ok(0) => return,
                            Ok(n) => {
                                remaining -= n as u64;
                                offset = (offset + n as u64) % block_len;
                                download.lane.moved();
                                if let Some(transfer) = &download.transfer {
                                    transfer.record(n);
                                }
                                if download.lane.due().is_some() {
                                    return;
                                }
                                since_yield += n as u64;
                                if since_yield >= YIELD_BYTES {
                                    since_yield = 0;
                                    tokio::task::yield_now().await;
                                }
                            }
                            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                                since_yield = 0;
                                let ready = tokio::select! {
                                    biased;
                                    _ = &mut ended => return,
                                    res = socket.writable() => res,
                                };
                                if ready.is_err() {
                                    return;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    let _ = download.lane.finish();
                    if close {
                        return;
                    }
                    continue;
                }
                ParsedFastHead::Upload {
                    request,
                    content_length,
                    close,
                    body_offset,
                } => {
                    let mut upload =
                        match self.app.admit_fast_upload(&request, &connection, exchange) {
                            Ok(upload) => upload,
                            Err(refusal) => {
                                let wire = encode_h1_response(&refusal, true);
                                let _ = timeout(EXCHANGE_BOUND, socket.write_all(&wire)).await;
                                return;
                            }
                        };

                    let trailing_len = buf.len() - body_offset;
                    let initial = (trailing_len as u64).min(content_length) as usize;
                    if initial > 0 {
                        upload.sink.record(initial);
                    }
                    if trailing_len > initial {
                        let leftover_start = body_offset + initial;
                        buf.copy_within(leftover_start.., 0);
                        buf.truncate(trailing_len - initial);
                    } else {
                        buf.clear();
                    }

                    let mut remaining = content_length - initial as u64;
                    if discard.capacity() < (256 << 10) {
                        discard = Vec::with_capacity(256 << 10);
                    }
                    let mut bulk_read = if remaining > UPLOAD_READ_MIN {
                        BulkRead::sample()
                    } else {
                        BulkRead::Ordinary
                    };
                    let mut since_yield: u64 = 0;
                    let ending = {
                        let mut ended = pin!(upload.lane.ended());
                        loop {
                            if remaining == 0 {
                                break upload.lane.finish();
                            }
                            if bulk_read.is_batched() && remaining <= bulk_read.lowat() {
                                let _ = bulk_read.finish(&socket);
                            }
                            let cap = (remaining as usize).min(discard.capacity());
                            let read = socket.try_io(Interest::READABLE, || {
                                let (_, len) = recv(
                                    &socket,
                                    &mut discard.spare_capacity_mut()[..cap],
                                    RecvFlags::TRUNC,
                                )?;
                                Ok(len)
                            });
                            match read {
                                Ok(0) => {
                                    let _ = bulk_read.finish(&socket);
                                    return;
                                }
                                Ok(n) => {
                                    let n = n.min(cap);
                                    upload.sink.record(n);
                                    remaining -= n as u64;
                                    bulk_read.on_read(n);
                                    if let Some(ending) = upload.lane.due() {
                                        break ending;
                                    }
                                    since_yield += n as u64;
                                    if since_yield >= YIELD_BYTES {
                                        since_yield = 0;
                                        tokio::task::yield_now().await;
                                    }
                                }
                                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                    since_yield = 0;
                                    if !bulk_read.is_batched() {
                                        bulk_read.on_would_block(&socket, remaining);
                                    }
                                    if bulk_read.is_batched() {
                                        let waited = tokio::select! {
                                            biased;
                                            ending = &mut ended => break ending,
                                            ready = timeout(BulkRead::WAIT, socket.readable()) => ready,
                                        };
                                        match waited {
                                            Ok(Ok(())) => {}
                                            Ok(Err(_)) => {
                                                let _ = bulk_read.finish(&socket);
                                                return;
                                            }
                                            Err(_) => bulk_read.reset_after_timeout(&socket),
                                        }
                                    } else {
                                        let waited = tokio::select! {
                                            biased;
                                            ending = &mut ended => break ending,
                                            ready = socket.readable() => ready,
                                        };
                                        if waited.is_err() {
                                            let _ = bulk_read.finish(&socket);
                                            return;
                                        }
                                    }
                                }
                                Err(_) => {
                                    let _ = bulk_read.finish(&socket);
                                    return;
                                }
                            }
                        }
                    };
                    let _ = bulk_read.finish(&socket);

                    let Some(answer) = self.app.finish_fast_upload(upload, &connection, ending)
                    else {
                        return;
                    };
                    let resp_close = close
                        || ending != LaneEnding::Finished
                        || remaining > 0
                        || answer
                            .headers()
                            .get(header::CONNECTION)
                            .and_then(|v| v.to_str().ok())
                            .is_some_and(|v| v.eq_ignore_ascii_case("close"));
                    let wire = encode_h1_response(&answer, resp_close);
                    if timeout(EXCHANGE_BOUND, socket.write_all(&wire))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if resp_close {
                        return;
                    }
                }
            }
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
    let bulk = if cfg!(target_os = "linux")
        && request.method() == Method::POST
        && request.uri().path() == Route::Upload.path()
    {
        http_body::Body::size_hint(request.body())
            .exact()
            .filter(|bytes| *bytes >= 1 << 20)
    } else {
        None
    };
    let upgrade = hyper::upgrade::on(&mut request);
    let exchange = shared.exchange(bulk);
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
    fn exchange(&self, bulk: Option<u64>) -> Exchange {
        let started = lock(&self.started).take();
        let exchange = Exchange::until(started.unwrap_or_else(|| Instant::now() + EXCHANGE_BOUND));
        self.post(State::Exchange(Some(exchange.watch()), started.and(bulk)));
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
    read_before_admission: u64,
    #[cfg(target_os = "linux")]
    bulk_read: BulkRead,
    #[cfg(target_os = "linux")]
    sendfile: Option<Sendfile>,
}

enum State {
    /// Between requests until the deadline; a first byte starts an exchange.
    Waiting,
    /// A request until admitted or answered by the deadline; its watch once its head is parsed.
    Exchange(Option<Watch>, Option<u64>),
    Reply(ReplyBound),
    /// Admitted work reading its request, or an upgraded connection.
    Open,
    Upload(u64),
}

impl<S> Io<S> {
    fn new(
        inner: S,
        shared: Arc<Shared>,
        deadline: Instant,
        #[cfg(target_os = "linux")] sendfile: Option<Sendfile>,
    ) -> Self {
        *lock(&shared.started) = Some(deadline);
        let (state, deadline) = (State::Exchange(None, None), Box::pin(sleep_until(deadline)));
        Self {
            inner,
            state,
            deadline,
            shared,
            burst: Burst::default(),
            feed: false,
            read_before_admission: 0,
            #[cfg(target_os = "linux")]
            bulk_read: BulkRead::default(),
            #[cfg(target_os = "linux")]
            sendfile,
        }
    }

    /// Adopts a posted limit and fails once the connection's deadline passed or its reply must end.
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()>
    where
        S: Rehome,
    {
        if self.shared.changed.load(Ordering::Acquire)
            && self.shared.changed.swap(false, Ordering::Acquire)
        {
            self.adopt()?;
        }
        match &mut self.state {
            State::Reply(bound) => Ok(bound.check(cx)?),
            State::Open | State::Upload(_) => Ok(()),
            // A request admitted while its body is read is bound by its lane instead.
            State::Exchange(Some(watch), bulk) if watch.admitted().is_some() => {
                let remaining = bulk.unwrap_or(0).saturating_sub(self.read_before_admission);
                #[cfg(target_os = "linux")]
                let batch = S::PLAIN && remaining > UPLOAD_READ_MIN;
                #[cfg(not(target_os = "linux"))]
                let batch = false;
                self.state = if batch {
                    #[cfg(target_os = "linux")]
                    {
                        self.bulk_read = BulkRead::sample();
                    }
                    State::Upload(remaining)
                } else {
                    State::Open
                };
                Ok(())
            }
            State::Waiting | State::Exchange(..)
                if self.deadline.as_mut().poll(cx).is_pending() =>
            {
                Ok(())
            }
            State::Waiting | State::Exchange(..) => Err(io::ErrorKind::TimedOut.into()),
        }
    }

    fn adopt(&mut self) -> io::Result<()>
    where
        S: Rehome,
    {
        let Some(state) = lock(&self.shared.posted).take() else {
            return Ok(());
        };
        #[cfg(target_os = "linux")]
        if S::PLAIN && matches!(self.state, State::Upload(_)) {
            self.bulk_read.finish(self.inner.socket())?;
        }
        if let State::Exchange(Some(watch), _) = &state {
            self.deadline.as_mut().reset(watch.deadline());
        }
        self.state = state;
        Ok(())
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

impl<S: AsyncRead + Rehome + Unpin> AsyncRead for Io<S> {
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
        let moved = buffer.filled().len() - filled;
        if let State::Upload(remaining) = &mut this.state {
            *remaining = remaining.saturating_sub(moved as u64);
            #[cfg(target_os = "linux")]
            if *remaining <= this.bulk_read.lowat() {
                this.bulk_read.finish(this.inner.socket())?;
                this.state = State::Open;
            } else {
                this.bulk_read.observe(
                    this.inner.socket(),
                    cx,
                    this.deadline.as_mut(),
                    moved,
                    result.is_pending(),
                    *remaining,
                )?;
            }
        } else if cfg!(target_os = "linux") && S::PLAIN && matches!(this.state, State::Exchange(..))
        {
            this.read_before_admission += moved as u64;
        }
        this.burst.moved(moved);
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
                this.state = State::Exchange(None, None);
                this.read_before_admission = moved as u64;
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

#[cfg(target_os = "linux")]
fn poll_sendfile(
    socket: &TcpStream,
    cx: &mut Context<'_>,
    file: &std::os::fd::OwnedFd,
    mut offset: u64,
    len: usize,
) -> Poll<io::Result<usize>> {
    use tokio::io::Interest;
    loop {
        ready!(socket.poll_write_ready(cx))?;
        let sent = socket.try_io(Interest::WRITABLE, || {
            Ok(rustix::fs::sendfile(socket, file, Some(&mut offset), len)?)
        });
        match sent {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            sent => return Poll::Ready(sent),
        }
    }
}

/// Sends any non-empty framing slices before a `Block` slice with `MSG_MORE`, then zero-copies the `Block` slice.
#[cfg(target_os = "linux")]
fn poll_sendfile_vectored(
    socket: &TcpStream,
    cx: &mut Context<'_>,
    prefix: &[io::IoSlice<'_>],
    file: &std::os::fd::OwnedFd,
    mut offset: u64,
    len: usize,
) -> Poll<io::Result<usize>> {
    use rustix::net::{SendFlags, send};
    use tokio::io::Interest;
    loop {
        ready!(socket.poll_write_ready(cx))?;
        let sent = socket.try_io(Interest::WRITABLE, || {
            let mut written = 0usize;
            for slice in prefix {
                let mut rem: &[u8] = slice;
                while !rem.is_empty() {
                    match send(socket, rem, SendFlags::MORE | SendFlags::NOSIGNAL) {
                        Ok(0) => return Ok(written),
                        Ok(n) => {
                            written += n;
                            rem = &rem[n..];
                        }
                        Err(err) => {
                            let err: io::Error = err.into();
                            return if written > 0 && err.kind() == io::ErrorKind::WouldBlock {
                                Ok(written)
                            } else {
                                Err(err)
                            };
                        }
                    }
                }
            }
            match rustix::fs::sendfile(socket, file, Some(&mut offset), len) {
                Ok(n) => Ok(written + n),
                Err(err) => {
                    let err: io::Error = err.into();
                    if written > 0 && err.kind() == io::ErrorKind::WouldBlock {
                        Ok(written)
                    } else {
                        Err(err)
                    }
                }
            }
        });
        match sent {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            sent => return Poll::Ready(sent),
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
        let result = if this.feed && Feed::<S>::RECORDS {
            Feed::poll_send(&mut this.inner, cx, &[io::IoSlice::new(bytes)])
        } else {
            #[cfg(target_os = "linux")]
            let sent = if S::PLAIN
                && let Some((file, offset)) = this.sendfile.as_ref().and_then(|sf| sf.locate(bytes))
            {
                Some(poll_sendfile(
                    this.inner.socket(),
                    cx,
                    file,
                    offset,
                    bytes.len(),
                ))
            } else {
                None
            };
            #[cfg(not(target_os = "linux"))]
            let sent = None;
            match sent {
                Some(result) => result,
                None => Pin::new(&mut this.inner).poll_write(cx, bytes),
            }
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
        let result = if this.feed && Feed::<S>::RECORDS {
            Feed::poll_send(&mut this.inner, cx, bytes)
        } else {
            #[cfg(target_os = "linux")]
            let sent = if S::PLAIN
                && let Some(sf) = this.sendfile.as_ref()
                && let Some((idx, (file, offset))) = bytes
                    .iter()
                    .enumerate()
                    .find_map(|(i, slice)| Some((i, sf.locate(slice)?)))
            {
                let prefix = &bytes[..idx];
                if prefix.iter().all(|s| s.is_empty()) {
                    Some(poll_sendfile(
                        this.inner.socket(),
                        cx,
                        file,
                        offset,
                        bytes[idx].len(),
                    ))
                } else {
                    Some(poll_sendfile_vectored(
                        this.inner.socket(),
                        cx,
                        prefix,
                        file,
                        offset,
                        bytes[idx].len(),
                    ))
                }
            } else {
                None
            };
            #[cfg(not(target_os = "linux"))]
            let sent = None;
            match sent {
                Some(result) => result,
                None => Pin::new(&mut this.inner).poll_write_vectored(cx, bytes),
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lane::Work, limits::Quota, peer::ClientKeys};
    use std::{task::Waker, time::Duration};
    use tokio_util::sync::CancellationToken;

    #[tokio::test(start_paused = true)]
    async fn an_admitted_exchange_leaves_its_deadline_at_once_and_an_unadmitted_one_times_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let _peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (socket, _) = listener.accept().unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = TcpStream::from_std(socket).unwrap();
        let mut cx = Context::from_waker(Waker::noop());
        let shared = Arc::new(Shared::default());
        let mut io = Io::new(
            socket,
            shared.clone(),
            Instant::now() + EXCHANGE_BOUND,
            #[cfg(target_os = "linux")]
            None,
        );
        let hold = Quota::new(1, 1).acquire(&ClientKeys::Exempt, 1).unwrap();
        let _lane = shared.exchange(None).admit(
            ClientKeys::Exempt,
            hold,
            Duration::from_secs(3600),
            &Work::default(),
            &CancellationToken::new(),
            None,
        );
        io.check(&mut cx).unwrap();
        assert!(
            matches!(io.state, State::Open),
            "admitted before the deadline"
        );

        let shared = Arc::new(Shared::default());
        let mut io = Io::new(
            io.inner,
            shared.clone(),
            Instant::now() + EXCHANGE_BOUND,
            #[cfg(target_os = "linux")]
            None,
        );
        let _unadmitted = shared.exchange(None);
        io.check(&mut cx).unwrap();
        tokio::time::advance(EXCHANGE_BOUND).await;
        let expired = io.check(&mut cx).unwrap_err();
        assert_eq!(expired.kind(), io::ErrorKind::TimedOut);
    }
}
