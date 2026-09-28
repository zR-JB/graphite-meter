//! HTTP/3 adapts streams to the same authorized measurement dispatcher.
use super::{http_quic::ReceiveCredit, *};
use graphite_meter_http3::{self as http3, RecvHalf, RequestStream, SendHalf};

const DATA_BYTES: usize = 16 * 1024;
const FAIRNESS_LANES: usize = 16;

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
                operations: operations.clone(),
                funded: false,
            };
            let connection = Connection {
                peer,
                tls: true,
                listener: Listener {
                    ui: false,
                    webtransport: true,
                },
            };
            let response = self
                .respond_incoming(request.map(|()| body), connection, &operations, None)
                .await?;
            respond(&mut send, response, head, active_responses).await
        };
        self.guard(&operations, &work, exchange).await
    }
}

struct RequestBody {
    stream: RecvHalf,
    finished: bool,
    credit: ReceiveCredit,
    operations: Operations,
    funded: bool,
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = http3::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, http3::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        if !self.funded {
            self.funded = holds_permit(&self.operations) && self.credit.fund();
        }
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

async fn respond(
    send: &mut SendHalf,
    response: Response<ResponseBody>,
    head: bool,
    active_responses: Arc<AtomicUsize>,
) -> io::Result<()> {
    let (parts, mut body) = response.into_parts();
    let active = (!head && body.size_hint().upper().is_some_and(|size| size > 1024 * 1024))
        .then(|| ActiveResponse::new(active_responses));
    // Go's idle writer, from the head on: a write the peer's stream credit holds for thirty seconds ends the reply.
    // Each write gets a fresh bound, and the operation's lifetime still caps them all.
    let written = |result: Result<Result<(), http3::Error>, tokio::time::error::Elapsed>| {
        result
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
            .map_err(io::Error::other)
    };
    written(tokio::time::timeout(WRITE_IDLE, send.send_response(Response::from_parts(parts, ()))).await)?;
    if !head {
        while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
            if let Ok(mut data) = frame?.into_data() {
                while !data.is_empty() {
                    let chunk = data.split_to(data.len().min(DATA_BYTES));
                    written(tokio::time::timeout(WRITE_IDLE, send.send_data(chunk)).await)?;
                    if active.as_ref().is_some_and(ActiveResponse::contended) {
                        // Only a crowded connection needs a scheduler
                        // handoff; per-chunk yields halve ordinary H3
                        // download throughput on this workload.
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }
    // The layer resets a response it has not finished, so a failure above never reads as complete.
    send.finish().await.map_err(io::Error::other)
}
