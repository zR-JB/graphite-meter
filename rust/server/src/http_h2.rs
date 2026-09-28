//! Multiplexed HTTP/2 transport with owned, independently cancellable streams.
use super::*;
use futures_util::{Stream, stream::FuturesUnordered};
use h2::{Reason, RecvStream, SendStream, server::SendResponse};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// TLS records and deframer, h2 frame reads, write buffer, HPACK and default window.
const TRANSPORT_BYTES: usize = 512 * 1024;
const STATE_BYTES: usize = 1024 * 1024;
pub(super) const BUFFER_BYTES: u32 = (TRANSPORT_BYTES + STATE_BYTES) as u32;
const DEFAULT_WINDOW_BYTES: u32 = 65_535;
const WINDOW_BYTES: u32 = 16 * 1024 * 1024;
const MAX_STREAMS: u32 = 250;
const FRAME_BYTES: usize = 16 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
type StreamFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

impl HttpServer {
    pub(super) async fn serve_http2_connection<T>(self: Arc<Self>, stream: T, facts: Connection)
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        // Until an upload is admitted, the default connection window bounds streams.
        let mut builder = h2::server::Builder::new();
        builder
            .initial_window_size(8 * 1024 * 1024)
            .max_frame_size(FRAME_BYTES as u32)
            .max_header_list_size(MAX_HEADER_BYTES as u32)
            .max_concurrent_streams(MAX_STREAMS)
            .max_send_buffer_size(FRAME_BYTES)
            .data_frame_budget(STATE_BYTES)
            .shared_budget(self.memory.clone(), STATE_BYTES);
        let stream = WriteProgressIo::new(stream, Duration::from_secs(30));
        let Ok(Ok(mut connection)) =
            tokio::time::timeout(Duration::from_secs(10), builder.handshake::<_, Bytes>(stream)).await
        else {
            return;
        };
        // Poll stream futures in their connection's scope rather than spawning
        // detached tasks. Dropping this scope synchronously drops every stream.
        let mut streams = FuturesUnordered::<StreamFuture>::new();
        let window = Arc::new(UploadWindow {
            memory: self.memory.clone(),
            uploads: AtomicUsize::new(0),
            granted: AtomicBool::new(false),
            work: AdmittedWork::new(),
        });
        let mut last_idle = None;
        let mut stale: Option<Pin<Box<Sleep>>> = None;
        let mut idle = Some(Box::pin(tokio::time::sleep(IDLE_TIMEOUT)));
        let mut closing: Option<Pin<Box<Sleep>>> = None;
        let mut stopping = Box::pin(stopped(self.stopping.clone()));
        let mut shutting_down = false;
        std::future::poll_fn(|cx| {
            while let Poll::Ready(Some(())) = Pin::new(&mut streams).poll_next(cx) {}
            let idle_since = window.work.idle_since();
            if idle_since != last_idle {
                last_idle = idle_since;
                let granted = window.granted.load(Ordering::Relaxed);
                stale = idle_since
                    .filter(|_| granted)
                    .map(|since| Box::pin(tokio::time::sleep_until(since + IDLE_TIMEOUT)));
                if let (Some(since), Some(deadline)) = (idle_since, closing.as_mut())
                    && !shutting_down
                {
                    deadline.as_mut().reset(since + SHUTDOWN_GRACE);
                }
            }
            let expired = stale.as_mut().is_some_and(|stale| stale.as_mut().poll(cx).is_ready());
            shutting_down = shutting_down || stopping.as_mut().poll(cx).is_ready();
            if closing.is_none() && (expired || shutting_down) {
                connection.graceful_shutdown();
                closing = Some(Box::pin(tokio::time::sleep(SHUTDOWN_GRACE)));
            }
            match connection.poll_accept(cx) {
                Poll::Ready(Some(Ok((request, mut reply)))) => {
                    idle = None;
                    if streams.len() >= MAX_STREAMS as usize {
                        reply.send_reset(Reason::REFUSED_STREAM);
                    } else {
                        let server = self.clone();
                        let window = window.clone();
                        streams.push(Box::pin(async move {
                            server.serve_http2_stream(request, reply, facts, window).await;
                        }));
                    }
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Ready(Some(Err(_)) | None) => Poll::Ready(()),
                Poll::Pending => {
                    if let Some(deadline) = &mut closing {
                        // Admitted work that raced the GOAWAY runs on, then gets the grace to drain.
                        return if idle_since.is_some() || shutting_down {
                            deadline.as_mut().poll(cx)
                        } else {
                            Poll::Pending
                        };
                    }
                    // Transport state includes queued END_STREAM frames even
                    // after the endpoint future completes. Neither active work
                    // nor queued output counts as an idle connection.
                    if !streams.is_empty() || connection.has_streams() {
                        idle = None;
                        return Poll::Pending;
                    }
                    let deadline = idle.get_or_insert_with(|| Box::pin(tokio::time::sleep(IDLE_TIMEOUT)));
                    if deadline.as_mut().poll(cx).is_ready() {
                        connection.graceful_shutdown();
                        closing = Some(Box::pin(tokio::time::sleep(SHUTDOWN_GRACE)));
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
        facts: Connection,
        window: Arc<UploadWindow>,
    ) {
        let head = request.method() == Method::HEAD;
        let operations = Arc::new(Mutex::new(Vec::new()));
        let work = window.work.clone();
        let exchange = async {
            let request = request.map(|stream| H2Body {
                stream,
                operations: operations.clone(),
                window,
                funded: false,
            });
            let response = self.respond_incoming(request, facts, &operations, None).await?;
            send_response(&mut reply, response, head).await
        };
        if self.guard(&operations, &work, exchange).await.is_err() {
            // Reset only this stream, including when peer flow control stopped
            // body polling. Healthy siblings retain their shared connection.
            reply.send_reset(Reason::CANCEL);
        }
    }
}

async fn send_response(
    reply: &mut SendResponse<Bytes>,
    response: Response<ResponseBody>,
    head: bool,
) -> io::Result<()> {
    let (parts, mut body) = response.into_parts();
    let finished = head || body.is_end_stream();
    let mut stream = reply
        .send_response(Response::from_parts(parts, ()), finished)
        .map_err(io::Error::other)?;
    if finished {
        return Ok(());
    }
    loop {
        let frame = tokio::select! {
            reset = std::future::poll_fn(|cx| stream.poll_reset(cx)) => {
                return Err(io::Error::other(format!("HTTP/2 stream reset: {reset:?}")));
            }
            frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)) => frame,
        };
        let Some(frame) = frame else {
            stream.send_data(Bytes::new(), true).map_err(io::Error::other)?;
            return Ok(());
        };
        let frame = frame?;
        if let Ok(mut data) = frame.into_data() {
            while !data.is_empty() {
                // Capacity is both peer flow control and h2's per-stream buffer
                // budget. Never enqueue a whole large body frame speculatively.
                let length = data.len().min(FRAME_BYTES);
                let capacity = reserve(&mut stream, length).await?;
                let chunk = data.split_to(length.min(capacity));
                let finished = data.is_empty() && body.is_end_stream();
                stream.send_data(chunk, finished).map_err(io::Error::other)?;
                if finished {
                    return Ok(());
                }
            }
        }
    }
}

async fn reserve(stream: &mut SendStream<Bytes>, bytes: usize) -> io::Result<usize> {
    stream.reserve_capacity(bytes);
    if stream.capacity() > 0 {
        return Ok(stream.capacity());
    }
    std::future::poll_fn(|cx| stream.poll_capacity(cx))
        .await
        .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
        .map_err(io::Error::other)
}

struct UploadWindow {
    memory: Arc<budget::MemoryBudget>,
    uploads: AtomicUsize,
    granted: AtomicBool,
    work: AdmittedWork,
}

struct H2Body {
    stream: RecvStream,
    operations: Operations,
    window: Arc<UploadWindow>,
    funded: bool,
}

impl Body for H2Body {
    type Data = Bytes;
    type Error = h2::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, h2::Error>>> {
        let this = &mut *self;
        // Under pressure an admitted upload keeps reading at the current window.
        if !this.funded && this.window.memory.has_headroom() && holds_permit(&this.operations) {
            let window = &this.window;
            this.funded = window.uploads.fetch_add(1, Ordering::Relaxed) > 0
                || this
                    .stream
                    .flow_control()
                    .set_target_connection_window_size(WINDOW_BYTES);
            if this.funded {
                window.granted.store(true, Ordering::Relaxed);
            } else {
                window.uploads.fetch_sub(1, Ordering::Relaxed);
            }
        }
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
        if self.funded && self.window.uploads.fetch_sub(1, Ordering::Relaxed) == 1 {
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
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
    use tokio::net::TcpStream;

    struct Served {
        server: Arc<HttpServer>,
        address: SocketAddr,
        connector: tokio_rustls::TlsConnector,
        stop: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<Result<(), ConfigError>>,
    }

    impl Served {
        async fn start(memory: usize) -> Self {
            let (certificate, key) = crate::test_identity::generate_identity("localhost").unwrap();
            let certificate = CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap();
            let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let tls = rustls::ServerConfig::builder_with_provider(provider.clone())
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![certificate.clone()], key)
                .unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(certificate).unwrap();
            let mut client = rustls::ClientConfig::builder_with_provider(provider)
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            client.alpn_protocols = vec![b"h2".to_vec()];
            let server = Arc::new(HttpServer::with_memory(Arc::new(Config::default()), memory).unwrap());
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
            let stream = self
                .connector
                .connect(
                    ServerName::try_from("localhost").unwrap(),
                    TcpStream::connect(self.address).await.unwrap(),
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
            assert!(idle - served.available() <= BUFFER_BYTES as usize);
        }
        assert!(refused > 0, "the flood never reached the connection's cap");
        let mut sibling = served.client(65_535).await;
        let (probe, _) = sibling.send_request(request(Method::GET, "/probe"), true).unwrap();
        assert_eq!(json(probe).await["protocolNegotiated"], "h2");
        drop((flood, sibling));
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
