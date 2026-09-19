//! Multiplexed HTTP/2 transport with owned, independently cancellable streams.
use super::*;
use futures_util::{Stream, stream::FuturesUnordered};
use h2::{Reason, RecvStream, SendStream, server::SendResponse};

const MAX_STREAMS: u32 = 256;
const FRAME_BYTES: usize = 16 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
type StreamFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

impl HttpServer {
    pub(super) async fn serve_http2_connection<T>(self: Arc<Self>, stream: T, facts: Connection)
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        // h2 keeps queued END_STREAM frames in its concurrency count until
        // pending frames and buffered DATA drain (Stream::is_closed). Together
        // these limits bound queued DATA to 256 * 16 KiB, even after handlers
        // release admission. Header/codec/TLS buffers have separate bounds.
        let mut builder = h2::server::Builder::new();
        builder
            .max_frame_size(FRAME_BYTES as u32)
            .max_header_list_size(MAX_HEADER_BYTES as u32)
            .max_concurrent_streams(MAX_STREAMS)
            .max_send_buffer_size(FRAME_BYTES);
        let stream = WriteProgressIo::new(stream, self.config.max_operation_duration);
        let Ok(Ok(mut connection)) = tokio::time::timeout(
            Duration::from_secs(10),
            builder.handshake::<_, Bytes>(stream),
        )
        .await
        else {
            return;
        };
        // Poll stream futures in their connection's scope rather than spawning
        // detached tasks. Dropping this scope synchronously drops every stream.
        let mut streams = FuturesUnordered::<StreamFuture>::new();
        let mut idle = Some(Box::pin(tokio::time::sleep(IDLE_TIMEOUT)));
        let mut closing: Option<Pin<Box<Sleep>>> = None;
        std::future::poll_fn(|cx| {
            while let Poll::Ready(Some(())) = Pin::new(&mut streams).poll_next(cx) {}
            match connection.poll_accept(cx) {
                Poll::Ready(Some(Ok((request, mut reply)))) => {
                    idle = None;
                    if streams.len() >= MAX_STREAMS as usize {
                        reply.send_reset(Reason::REFUSED_STREAM);
                    } else {
                        let server = self.clone();
                        streams.push(Box::pin(async move {
                            server.serve_http2_stream(request, reply, facts).await;
                        }));
                    }
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Ready(Some(Err(_)) | None) => Poll::Ready(()),
                Poll::Pending => {
                    if let Some(deadline) = &mut closing {
                        return deadline.as_mut().poll(cx);
                    }
                    // Transport state includes queued END_STREAM frames even
                    // after the endpoint future completes. Neither active work
                    // nor queued output counts as an idle connection.
                    if !streams.is_empty() || connection.has_streams() {
                        idle = None;
                        return Poll::Pending;
                    }
                    let deadline =
                        idle.get_or_insert_with(|| Box::pin(tokio::time::sleep(IDLE_TIMEOUT)));
                    if deadline.as_mut().poll(cx).is_ready() {
                        connection.graceful_shutdown();
                        let mut deadline = Box::pin(tokio::time::sleep(SHUTDOWN_GRACE));
                        let _ = deadline.as_mut().poll(cx);
                        closing = Some(deadline);
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
    ) {
        let head = request.method() == Method::HEAD;
        let metered = request.method() != Method::OPTIONS
            && matches!(
                request.uri().path(),
                "/download" | "/upload" | "/upload/progress"
            );
        // This registry belongs only to this stream. Upload can register its
        // permit before body reception, without placing a timer on shared IO.
        let operations = Arc::new(Mutex::new(Vec::new()));
        let result = {
            let exchange = async {
                let response = self
                    .respond_incoming(request.map(H2Body), facts, &operations, None)
                    .await?;
                send_response(&mut reply, response, head).await
            };
            let mut exchange = std::pin::pin!(exchange);
            let guarded = std::future::poll_fn(|cx| {
                let check = || {
                    for operation in operations.lock().expect("operations poisoned").iter() {
                        operation.lock().expect("operation poisoned").check(cx)?;
                    }
                    Ok::<_, io::Error>(())
                };
                let mut check = check;
                if let Err(error) = check() {
                    return Poll::Ready(Err(error));
                }
                let result = exchange.as_mut().poll(cx);
                // Dispatch can install its lease during this poll. Register its
                // revocation wake even if flow control blocks before another poll.
                if result.is_pending() {
                    for operation in operations.lock().expect("operations poisoned").iter() {
                        if let Err(error) = operation.lock().expect("operation poisoned").check(cx)
                        {
                            return Poll::Ready(Err(error));
                        }
                    }
                }
                result
            });
            if metered {
                tokio::time::timeout(self.config.max_operation_duration, guarded)
                    .await
                    .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
            } else {
                guarded.await
            }
        };
        if result.is_err() {
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
            stream
                .send_data(Bytes::new(), true)
                .map_err(io::Error::other)?;
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
                stream
                    .send_data(chunk, finished)
                    .map_err(io::Error::other)?;
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

struct H2Body(RecvStream);
impl Body for H2Body {
    type Data = Bytes;
    type Error = h2::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, h2::Error>>> {
        match ready!(self.0.poll_data(cx)) {
            Some(Ok(data)) => {
                self.0.flow_control().release_capacity(data.len())?;
                Poll::Ready(Some(Ok(Frame::data(data))))
            }
            Some(Err(error)) => Poll::Ready(Some(Err(error))),
            None => Poll::Ready(None),
        }
    }
}

/// A stream window can stall independently of its siblings, but a blocked TLS
/// writer stalls the entire connection. Bound only actual pending IO, including
/// queued END_STREAM output whose endpoint future has already completed.
struct WriteProgressIo<T> {
    inner: T,
    timeout: Duration,
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<T> WriteProgressIo<T> {
    fn new(inner: T, timeout: Duration) -> Self {
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
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check_stall(cx)?;
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for WriteProgressIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
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
        assert!(matches!(
            poll_once(write.as_mut()).await,
            Poll::Ready(Ok(()))
        ));
        assert_eq!(reader.read_u8().await.unwrap(), b'c');
    }

    struct BufferedWriter {
        accepted: usize,
    }

    impl AsyncWrite for BufferedWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
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
        let mut writer =
            WriteProgressIo::new(BufferedWriter { accepted: 0 }, Duration::from_millis(20));
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
