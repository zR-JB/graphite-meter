//! HTTP/3 adapts streams to the same authorized measurement dispatcher.
use super::{
    body::{DataSink, UploadFunding, write_reply},
    quic::ReceiveCredit,
    *,
};
use graphite_meter_http3::{self as http3, RecvHalf, RequestStream, SendHalf};

const DATA_BYTES: usize = 16 * 1024;
const FAIRNESS_LANES: usize = 16;
/// Replies larger than this count toward a crowded connection's fairness lanes.
const LARGE_RESPONSE_BYTES: u64 = 1024 * 1024;

impl HttpServer {
    /// `peer` must be the actual accepted QUIC peer. QUIC supplies TLS; this
    /// native listener deliberately has no authority to serve login/UI routes.
    pub(super) async fn serve_http3_request(
        &self,
        request: Request<()>,
        stream: RequestStream,
        peer: SocketAddr,
        credit: ReceiveCredit,
        active_responses: Arc<AtomicUsize>,
    ) -> io::Result<()> {
        let head = request.method() == Method::HEAD;
        let (mut send, receive) = stream.split();
        let operations: Operations = Arc::new(Mutex::new(Vec::new()));
        let work = credit.work().clone();
        let exchange = async {
            let body = RequestBody {
                stream: receive,
                finished: false,
                credit,
                funding: UploadFunding::new(operations.clone()),
            };
            let request = request.map(|()| body);
            let response = self
                .respond_incoming(request, Accepted::quic(peer), &operations, None)
                .await?;
            let large = response
                .body()
                .size_hint()
                .upper()
                .is_some_and(|size| size > LARGE_RESPONSE_BYTES);
            let mut sink = H3Reply {
                send: &mut send,
                active: (!head && large).then(|| ActiveResponse::new(active_responses)),
            };
            write_reply(&mut sink, response, head).await
        };
        self.guard(&operations, &work, exchange).await
    }
}

struct RequestBody {
    stream: RecvHalf,
    finished: bool,
    credit: ReceiveCredit,
    funding: UploadFunding,
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = http3::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, http3::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        let this = &mut *self;
        this.funding.fund(|| true, |clients| this.credit.fund(clients));
        let frame = ready!(self.stream.poll_data(cx))
            .transpose()
            .map(|data| data.map(Frame::data));
        self.finished = !matches!(frame, Some(Ok(_)));
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.finished
    }
}

/// A large reply on its connection, whose writes yield to its siblings once more than the fairness lanes run.
struct ActiveResponse(Arc<AtomicUsize>);

impl ActiveResponse {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }

    fn contended(&self) -> bool {
        self.0.load(Ordering::Relaxed) > FAIRNESS_LANES
    }
}

impl Drop for ActiveResponse {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// An HTTP/3 request stream's reply, as Go's idle writer from the head on: a write the peer's stream credit holds
/// for the idle bound ends it. Each write gets a fresh bound, and the operation's lifetime still caps them all.
pub(super) struct H3Reply<'a> {
    pub(super) send: &'a mut SendHalf,
    active: Option<ActiveResponse>,
}

impl<'a> H3Reply<'a> {
    /// A reply that never yields to its siblings.
    pub(super) fn new(send: &'a mut SendHalf) -> Self {
        Self { send, active: None }
    }
}

impl DataSink for H3Reply<'_> {
    async fn head(&mut self, head: Response<()>, _end: bool) -> io::Result<bool> {
        written(tokio::time::timeout(IDLE_BOUND, self.send.send_response(head)).await)?;
        Ok(false)
    }

    async fn data(&mut self, data: &mut Bytes, _end: bool) -> io::Result<bool> {
        let chunk = data.split_to(data.len().min(DATA_BYTES));
        written(tokio::time::timeout(IDLE_BOUND, self.send.send_data(chunk)).await)?;
        if self.active.as_ref().is_some_and(ActiveResponse::contended) {
            // Only a crowded connection needs a scheduler
            // handoff; per-chunk yields halve ordinary H3
            // download throughput on this workload.
            tokio::task::yield_now().await;
        }
        Ok(false)
    }

    /// The layer resets a response it has not finished, so a failure before this never reads as complete.
    async fn finish(&mut self) -> io::Result<()> {
        self.send.finish().await.map_err(io::Error::other)
    }

    async fn cancelled(&mut self) -> io::Error {
        let error = match self.send.stopped().await {
            Ok(code) => http3::Error::Stopped(code.unwrap_or(http3::Code::H3_REQUEST_CANCELLED)),
            Err(error) => error,
        };
        io::Error::other(error)
    }
}

fn written(result: Result<Result<(), http3::Error>, tokio::time::error::Elapsed>) -> io::Result<()> {
    result
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
        .map_err(io::Error::other)
}
