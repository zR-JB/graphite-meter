//! Multiplexed HTTP/2 transport with owned, independently cancellable streams.
use super::{
    body::{DataSink, UploadFunding, write_reply},
    lifecycle::ConnectionLifecycle,
    *,
};
use crate::{
    budget::{ClientCredit, CreditClaim, H2_STATE_BYTES, MemoryBudget},
    timeouts::H2_HANDSHAKE,
};
use futures_util::{Stream, stream::FuturesUnordered};
use h2::{Reason, RecvStream, SendStream, server::SendResponse};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const DEFAULT_WINDOW_BYTES: u32 = 65_535;
/// Go's h2ReceiveWindowPerConnection, granted to a connection's first funded upload.
const WINDOW_BYTES: u32 = 16 * 1024 * 1024;
/// Go's h2ReceiveWindowPerStream.
const STREAM_WINDOW_BYTES: u32 = 8 * 1024 * 1024;
const MAX_STREAMS: u32 = 250;
const FRAME_BYTES: usize = 16 * 1024;

impl HttpServer {
    pub(super) async fn serve_http2_connection<T>(self: Arc<Self>, stream: T, accepted: Accepted)
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        // Until an upload is admitted, the default connection window bounds streams.
        let mut builder = h2::server::Builder::new();
        builder
            .initial_window_size(STREAM_WINDOW_BYTES)
            .max_frame_size(FRAME_BYTES as u32)
            .max_header_list_size(MAX_HEADER_BYTES as u32)
            .max_concurrent_streams(MAX_STREAMS)
            .max_send_buffer_size(FRAME_BYTES)
            .data_frame_budget(H2_STATE_BYTES)
            .shared_budget(self.memory.clone(), H2_STATE_BYTES);
        let stream = WriteProgressIo::new(stream, IDLE_BOUND);
        let Ok(Ok(mut connection)) = tokio::time::timeout(H2_HANDSHAKE, builder.handshake::<_, Bytes>(stream)).await
        else {
            return;
        };
        // Poll stream futures in their connection's scope rather than spawning
        // detached tasks. Dropping this scope synchronously drops every stream.
        let mut streams = FuturesUnordered::new();
        let window = Arc::new(UploadWindow {
            memory: self.memory.clone(),
            clients: self.client_credit.clone(),
            claim: Mutex::default(),
            uploads: AtomicUsize::new(0),
            granted: AtomicBool::new(false),
            work: AdmittedWork::new(),
        });
        let mut lifecycle = ConnectionLifecycle::new(window.work.clone(), true);
        let mut idle = Some(Box::pin(tokio::time::sleep(CONTROL)));
        let mut stopping = Box::pin(stopped(self.stopping.clone()));
        std::future::poll_fn(|cx| {
            while let Poll::Ready(Some(())) = Pin::new(&mut streams).poll_next(cx) {}
            let expired = lifecycle.poll_stale(cx, || window.granted.load(Ordering::Relaxed));
            if !lifecycle.stopping() && stopping.as_mut().poll(cx).is_ready() {
                lifecycle.stop();
            }
            if (expired || lifecycle.stopping()) && lifecycle.go_away() {
                connection.graceful_shutdown();
            }
            match connection.poll_accept(cx) {
                Poll::Ready(Some(Ok((request, mut reply)))) => {
                    idle = None;
                    if streams.len() >= MAX_STREAMS as usize {
                        reply.send_reset(Reason::REFUSED_STREAM);
                    } else {
                        let server = self.clone();
                        let window = window.clone();
                        streams.push(async move {
                            server.serve_http2_stream(request, reply, accepted, window).await;
                        });
                    }
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Ready(Some(Err(_)) | None) => Poll::Ready(()),
                Poll::Pending => {
                    // Admitted work that raced the GOAWAY runs on, then gets the grace to drain.
                    if lifecycle.going_away() {
                        return lifecycle.poll_close(cx);
                    }
                    // Transport state includes queued END_STREAM frames even
                    // after the endpoint future completes. Neither active work
                    // nor queued output counts as an idle connection.
                    if !streams.is_empty() || connection.has_streams() {
                        idle = None;
                        return Poll::Pending;
                    }
                    let deadline = idle.get_or_insert_with(|| Box::pin(tokio::time::sleep(CONTROL)));
                    if deadline.as_mut().poll(cx).is_ready() {
                        lifecycle.go_away();
                        connection.graceful_shutdown();
                        cx.waker().wake_by_ref();
                    }
                    Poll::Pending
                }
            }
        })
        .await;
    }

    async fn serve_http2_stream(
        &self,
        request: Request<RecvStream>,
        mut reply: SendResponse<Bytes>,
        accepted: Accepted,
        window: Arc<UploadWindow>,
    ) {
        let head = request.method() == Method::HEAD;
        let operations = Arc::new(Mutex::new(Vec::new()));
        let work = window.work.clone();
        let exchange = async {
            let request = request.map(|stream| H2Body {
                stream,
                window,
                funding: UploadFunding::new(operations.clone()),
            });
            let response = self.respond_incoming(request, accepted, &operations, None).await?;
            let mut sink = H2Reply {
                respond: &mut reply,
                stream: None,
            };
            write_reply(&mut sink, response, head).await
        };
        if self.guard(&operations, &work, exchange).await.is_err() {
            // Reset only this stream, including when peer flow control stopped
            // body polling. Healthy siblings retain their shared connection.
            reply.send_reset(Reason::CANCEL);
        }
    }
}

/// An HTTP/2 stream's reply. Its head and last data carry END_STREAM, and a peer's reset ends it.
struct H2Reply<'a> {
    respond: &'a mut SendResponse<Bytes>,
    /// Set by the head.
    stream: Option<SendStream<Bytes>>,
}

impl H2Reply<'_> {
    fn stream(&mut self) -> &mut SendStream<Bytes> {
        self.stream.as_mut().expect("a reply's head comes first")
    }
}

impl DataSink for H2Reply<'_> {
    async fn head(&mut self, head: Response<()>, end: bool) -> io::Result<bool> {
        self.stream = Some(self.respond.send_response(head, end).map_err(io::Error::other)?);
        Ok(end)
    }

    async fn data(&mut self, data: &mut Bytes, end: bool) -> io::Result<bool> {
        let stream = self.stream();
        // Capacity is both peer flow control and h2's per-stream buffer
        // budget. Never enqueue a whole large body frame speculatively.
        let length = data.len().min(FRAME_BYTES);
        let capacity = reserve(stream, length).await?;
        let chunk = data.split_to(length.min(capacity));
        let end = end && data.is_empty();
        stream.send_data(chunk, end).map_err(io::Error::other)?;
        Ok(end)
    }

    async fn finish(&mut self) -> io::Result<()> {
        self.stream().send_data(Bytes::new(), true).map_err(io::Error::other)
    }

    async fn cancelled(&mut self) -> io::Error {
        let stream = self.stream();
        let reset = std::future::poll_fn(|cx| stream.poll_reset(cx)).await;
        io::Error::other(format!("HTTP/2 stream reset: {reset:?}"))
    }
}

/// As Go's idle writer, a write the peer's flow control holds for the idle bound ends the reply; each write gets a
/// fresh bound, and the operation's lifetime still caps them all.
async fn reserve(stream: &mut SendStream<Bytes>, bytes: usize) -> io::Result<usize> {
    stream.reserve_capacity(bytes);
    if stream.capacity() > 0 {
        return Ok(stream.capacity());
    }
    tokio::time::timeout(IDLE_BOUND, std::future::poll_fn(|cx| stream.poll_capacity(cx)))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
        .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
        .map_err(io::Error::other)
}

struct UploadWindow {
    memory: Arc<MemoryBudget>,
    clients: Arc<ClientCredit>,
    /// Kept until the connection ends once its window was granted: the peer may fill it until then.
    claim: Mutex<Option<CreditClaim>>,
    uploads: AtomicUsize,
    granted: AtomicBool,
    work: AdmittedWork,
}

impl UploadWindow {
    /// Raises the connection window for its first funded upload, within the admitted client's share.
    fn raise(&self, stream: &mut RecvStream, clients: &[String]) -> bool {
        let mut claim = lock(&self.claim);
        if claim.is_none() {
            *claim = self
                .clients
                .claim(clients, (WINDOW_BYTES - DEFAULT_WINDOW_BYTES) as usize);
        }
        if claim.is_some() && stream.flow_control().set_target_connection_window_size(WINDOW_BYTES) {
            self.granted.store(true, Ordering::Relaxed);
            return true;
        }
        if !self.granted.load(Ordering::Relaxed) {
            *claim = None;
        }
        false
    }
}

struct H2Body {
    stream: RecvStream,
    window: Arc<UploadWindow>,
    funding: UploadFunding,
}

impl Body for H2Body {
    type Data = Bytes;
    type Error = h2::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, h2::Error>>> {
        let this = &mut *self;
        let (window, stream) = (&this.window, &mut this.stream);
        // Under pressure, or past its client's share, an admitted upload keeps reading at the current window.
        this.funding.fund(
            || window.memory.has_headroom(),
            |clients| {
                let funded = window.uploads.fetch_add(1, Ordering::Relaxed) > 0 || window.raise(stream, clients);
                if !funded {
                    window.uploads.fetch_sub(1, Ordering::Relaxed);
                }
                funded
            },
        );
        match ready!(this.stream.poll_data(cx)) {
            Some(Ok(data)) => {
                this.stream.flow_control().release_capacity(data.len())?;
                Poll::Ready(Some(Ok(Frame::data(data))))
            }
            Some(Err(error)) => Poll::Ready(Some(Err(error))),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.stream.is_end_stream()
    }
}

impl Drop for H2Body {
    fn drop(&mut self) {
        if self.funding.funded && self.window.uploads.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.stream
                .flow_control()
                .set_target_connection_window_size(DEFAULT_WINDOW_BYTES);
        }
    }
}

/// A stream window can stall independently of its siblings, but a blocked TLS
/// writer stalls the entire connection. Bound only actual pending IO, including
/// queued END_STREAM output whose endpoint future has already completed.
pub(super) struct WriteProgressIo<T> {
    inner: T,
    timeout: Duration,
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<T> WriteProgressIo<T> {
    pub(super) fn new(inner: T, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            stalled: None,
        }
    }

    fn check_stall(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self
            .stalled
            .as_mut()
            .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
        {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }

    fn pending_write(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let timeout = self.timeout;
        let timer = self
            .stalled
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
        if timer.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(io::ErrorKind::TimedOut.into()))
        } else {
            Poll::Pending
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for WriteProgressIo<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        self.check_stall(cx)?;
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for WriteProgressIo<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        self.check_stall(cx)?;
        match Pin::new(&mut self.inner).poll_write(cx, bytes) {
            Poll::Ready(result) => {
                if matches!(result, Ok(count) if count > 0) {
                    self.stalled = None;
                }
                Poll::Ready(result)
            }
            Poll::Pending => self.pending_write(cx).map_ok(|()| 0),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.check_stall(cx)?;
        match Pin::new(&mut self.inner).poll_write_vectored(cx, bytes) {
            Poll::Ready(result) => {
                if matches!(result, Ok(count) if count > 0) {
                    self.stalled = None;
                }
                Poll::Ready(result)
            }
            Poll::Pending => self.pending_write(cx).map_ok(|()| 0),
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_stall(cx)?;
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(result) => {
                if result.is_ok() {
                    self.stalled = None;
                }
                Poll::Ready(result)
            }
            Poll::Pending => self.pending_write(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_stall(cx)?;
        match Pin::new(&mut self.inner).poll_shutdown(cx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => self.pending_write(cx),
        }
    }
}

#[cfg(test)]
mod write_stall_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
        std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_pending_writes_do_not_extend_stall_deadline() {
        let (writer, _non_reading_peer) = tokio::io::duplex(1);
        let mut writer = WriteProgressIo::new(writer, Duration::from_millis(20));
        let write = writer.write_all(b"ab");
        tokio::pin!(write);
        assert!(poll_once(write.as_mut()).await.is_pending());
        for _ in 0..3 {
            tokio::time::advance(Duration::from_millis(5)).await;
            assert!(poll_once(write.as_mut()).await.is_pending());
        }
        tokio::time::advance(Duration::from_millis(5)).await;
        let Poll::Ready(Err(error)) = poll_once(write.as_mut()).await else {
            panic!("pending polls extended the blocked writer's deadline");
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn real_write_progress_starts_a_fresh_stall_period() {
        let (writer, mut reader) = tokio::io::duplex(1);
        let mut writer = WriteProgressIo::new(writer, Duration::from_millis(20));
        let write = writer.write_all(b"abc");
        tokio::pin!(write);
        assert!(poll_once(write.as_mut()).await.is_pending());
        tokio::time::advance(Duration::from_millis(15)).await;
        assert_eq!(reader.read_u8().await.unwrap(), b'a');
        assert!(poll_once(write.as_mut()).await.is_pending());
        // Thirty milliseconds total exceeds the original deadline, but the
        // second byte made real progress and must grant a fresh interval.
        tokio::time::advance(Duration::from_millis(15)).await;
        assert_eq!(reader.read_u8().await.unwrap(), b'b');
        assert!(matches!(poll_once(write.as_mut()).await, Poll::Ready(Ok(()))));
        assert_eq!(reader.read_u8().await.unwrap(), b'c');
    }

    struct BufferedWriter {
        accepted: usize,
    }

    impl AsyncWrite for BufferedWriter {
        fn poll_write(mut self: Pin<&mut Self>, _: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
            self.accepted += bytes.len();
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    #[tokio::test(start_paused = true)]
    async fn final_buffered_output_remains_bounded_until_transport_flush() {
        let mut writer = WriteProgressIo::new(BufferedWriter { accepted: 0 }, Duration::from_millis(20));
        // Model a completed response whose final frame was accepted into TLS's
        // buffer, while its encrypted output can no longer reach the socket.
        writer.write_all(b"final END_STREAM frame").await.unwrap();
        assert_eq!(writer.inner.accepted, 22);
        let flush = writer.flush();
        tokio::pin!(flush);
        assert!(poll_once(flush.as_mut()).await.is_pending());
        tokio::time::advance(Duration::from_millis(10)).await;
        assert!(poll_once(flush.as_mut()).await.is_pending());
        tokio::time::advance(Duration::from_millis(10)).await;
        let Poll::Ready(Err(error)) = poll_once(flush.as_mut()).await else {
            panic!("final queued response escaped the write-stall bound");
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::config::Config;
    use rustls::pki_types::ServerName;

    struct Served {
        server: Arc<HttpServer>,
        address: SocketAddr,
        connector: tokio_rustls::TlsConnector,
        stop: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<Result<(), ServerError>>,
    }

    impl Served {
        async fn start(memory: usize) -> Self {
            let (tls, client) = crate::test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"h2"]).unwrap();
            let server = Arc::new(HttpServer::with_memory(Config::default().validated().unwrap(), memory).unwrap());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let serving = server
                .clone()
                .serve(NativeKind::H2, listener, Some(Arc::new(tls)), async {
                    let _ = stopped.await;
                });
            let task = tokio::spawn(serving);
            Self {
                server,
                address,
                connector: tokio_rustls::TlsConnector::from(Arc::new(client)),
                stop,
                task,
            }
        }

        fn available(&self) -> usize {
            self.server.memory.available()
        }

        async fn client(&self, window: u32) -> h2::client::SendRequest<Bytes> {
            self.client_from([127, 0, 0, 1], window).await
        }

        async fn client_from(&self, source: [u8; 4], window: u32) -> h2::client::SendRequest<Bytes> {
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.bind(SocketAddr::from((source, 0))).unwrap();
            let stream = self
                .connector
                .connect(
                    ServerName::try_from("localhost").unwrap(),
                    socket.connect(self.address).await.unwrap(),
                )
                .await
                .unwrap();
            let (client, connection) = h2::client::Builder::new()
                .initial_window_size(window)
                .handshake(stream)
                .await
                .unwrap();
            tokio::spawn(connection);
            client.ready().await.unwrap()
        }

        async fn stop(self) {
            self.stop.send(()).unwrap();
            self.task.await.unwrap().unwrap();
        }
    }

    fn request(method: Method, path: &str) -> Request<()> {
        Request::builder()
            .method(method)
            .uri(format!("https://localhost{path}"))
            .body(())
            .unwrap()
    }

    async fn json(response: h2::client::ResponseFuture) -> serde_json::Value {
        let mut body = response.await.unwrap().into_body();
        let mut bytes = Vec::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk.unwrap();
            body.flow_control().release_capacity(chunk.len()).unwrap();
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn unadmitted_header_and_data_floods_stay_within_the_floor() {
        let served = Served::start(64 * 1024 * 1024).await;
        let idle = served.available();
        let mut flood = served.client(0).await;
        let pad = http::HeaderValue::from_bytes(&[b'p'; 24 * 1024]).unwrap();
        let mut held = Vec::new();
        for _ in 0..64 {
            flood = flood.ready().await.unwrap();
            let mut head = request(Method::POST, "/probe");
            head.headers_mut().insert("x-pad", pad.clone());
            held.push(flood.send_request(head, false).unwrap());
        }
        for (_, upload) in &mut held {
            for _ in 0..64 {
                upload.reserve_capacity(1);
                if upload.capacity() > 0 {
                    let _ = upload.send_data(Bytes::from_static(b"x"), false);
                }
            }
        }
        let mut refused = 0;
        for (response, _) in held {
            let response = tokio::time::timeout(Duration::from_secs(5), response).await;
            if !matches!(response, Ok(Ok(ref reply)) if reply.status() == StatusCode::OK) {
                refused += 1;
            }
            assert!(idle - served.available() <= crate::budget::H2_FLOOR_BYTES);
        }
        assert!(refused > 0, "the flood never reached the connection's cap");
        let mut sibling = served.client(65_535).await;
        let (probe, _) = sibling.send_request(request(Method::GET, "/probe"), true).unwrap();
        assert_eq!(json(probe).await["protocolNegotiated"], "h2");
        drop((flood, sibling));
        served.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_client_holds_at_most_its_share_of_receive_credit() {
        // A client's share is a window on each QUIC connection it may hold. While its other connections hold all of it
        // but three HTTP/2 windows, three more fit it and a fourth does not.
        let served = Served::start(2 * 1024 * 1024 * 1024).await;
        let window = (WINDOW_BYTES - DEFAULT_WINDOW_BYTES) as usize;
        let share = crate::connections::QUIC_PER_CLIENT * crate::budget::QUIC_CREDIT_BYTES;
        let keys = crate::client_address::client_keys([127, 0, 0, 1].into());
        let _others = served.server.client_credit.claim(&keys, share - 3 * window).unwrap();
        let mut held = Vec::new();
        let mut funded = Vec::new();
        for source in [1, 1, 1, 1, 2] {
            let mut client = served.client_from([127, 0, 0, source], 65_535).await;
            let (session, _) = client
                .send_request(request(Method::POST, "/upload/session"), true)
                .unwrap();
            let id = json(session).await["uploadId"].as_str().unwrap().to_owned();
            let before = served.available();
            client = client.ready().await.unwrap();
            let (reply, mut upload) = client
                .send_request(request(Method::POST, &format!("/upload?id={id}")), false)
                .unwrap();
            upload.send_data(Bytes::from_static(b"x"), false).unwrap();
            // Once the receiver has counted the byte, the admitted upload has asked for its window.
            loop {
                client = client.ready().await.unwrap();
                let checkpoint = request(Method::POST, &format!("/upload/checkpoint?id={id}"));
                let (checkpoint, _) = client.send_request(checkpoint, true).unwrap();
                if json(checkpoint).await["bytes"] == 1 {
                    break;
                }
            }
            funded.push(before - served.available() >= window);
            held.push((client, reply, upload));
        }
        assert_eq!(funded, [true, true, true, false, true]);
        drop(held);
        served.stop().await;
    }

    #[tokio::test]
    async fn pressure_keeps_admitted_uploads_at_the_default_window() {
        let limit = 80 * 1024 * 1024;
        let served = Served::start(limit).await;
        let idle = served.available();
        let mut client = served.client(65_535).await;
        let pressure = served.server.memory.lease(served.available() - limit / 4).unwrap();
        let held = served.available();
        let (session, _) = client
            .send_request(request(Method::POST, "/upload/session"), true)
            .unwrap();
        let id = json(session).await["uploadId"].as_str().unwrap().to_owned();
        client = client.ready().await.unwrap();
        let (reply, mut upload) = client
            .send_request(request(Method::POST, &format!("/upload?id={id}")), false)
            .unwrap();
        let mut body = Bytes::from(vec![7; 512 * 1024]);
        while !body.is_empty() {
            upload.reserve_capacity(body.len());
            let capacity = std::future::poll_fn(|cx| upload.poll_capacity(cx))
                .await
                .unwrap()
                .unwrap();
            assert!(capacity <= DEFAULT_WINDOW_BYTES as usize, "window grew under pressure");
            assert_eq!(served.available(), held);
            let chunk = body.split_to(capacity.min(body.len()));
            upload.send_data(chunk, body.is_empty()).unwrap();
        }
        assert_eq!(json(reply).await["bytes"], 512 * 1024);
        client = client.ready().await.unwrap();
        let (probe, _) = client.send_request(request(Method::GET, "/probe"), true).unwrap();
        assert_eq!(json(probe).await["load"]["active"], 0);
        drop((client, pressure));
        let server = served.server.clone();
        served.stop().await;
        assert_eq!(server.memory.available(), idle);
    }
}
