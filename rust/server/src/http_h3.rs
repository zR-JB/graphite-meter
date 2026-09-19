//! HTTP/3 adapts streams to the same authorized measurement dispatcher.
use super::*;
use bytes::Buf;
use h3::error::Code;

pub type Http3RequestStream = h3::server::RequestStream<h3_noq::BidiStream<Bytes>, Bytes>;
type Receive = h3::server::RequestStream<h3_noq::RecvStream, Bytes>;
type Send = h3::server::RequestStream<h3_noq::SendStream<Bytes>, Bytes>;
const DATA_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Http3RequestKind {
    Measurement,
    WebTransport,
    InvalidConnect,
}

impl HttpServer {
    /// Classification consumes neither credentials nor request DATA. The
    /// connection owner hands valid CONNECT requests to its session engine,
    /// which must authorize them before accepting the session.
    pub fn classify_http3_request(request: &Request<()>) -> Http3RequestKind {
        if request.method() != Method::CONNECT {
            Http3RequestKind::Measurement
        } else if request.extensions().get::<h3::ext::Protocol>()
            == Some(&h3::ext::Protocol::WEB_TRANSPORT)
        {
            Http3RequestKind::WebTransport
        } else {
            Http3RequestKind::InvalidConnect
        }
    }

    /// Serve an already resolved ordinary request. The connection owner must
    /// bound header resolution to 10s, field sections to 32KiB, and the number
    /// of owned request futures to 256. No stream task is spawned here.
    ///
    /// `peer` must be the actual accepted QUIC peer. QUIC supplies TLS; this
    /// native listener deliberately has no authority to serve login/UI routes.
    pub async fn serve_http3_request(
        &self,
        request: Request<()>,
        stream: Http3RequestStream,
        peer: SocketAddr,
    ) -> io::Result<()> {
        let head = request.method() == Method::HEAD;
        let connect = request.method() == Method::CONNECT;
        let (send, receive) = stream.split();
        let mut send = ResponseStream {
            stream: send,
            finished: false,
        };
        let operations: Operations = Arc::new(Mutex::new(Vec::new()));
        let exchange = async {
            let body = RequestBody {
                stream: receive,
                finished: false,
            };
            let response = if connect {
                // CONNECT belongs to the session dispatcher. A mistaken call
                // cannot execute an ordinary measurement under CONNECT.
                text_response(StatusCode::BAD_REQUEST)
            } else {
                self.respond_incoming(
                    request.map(|()| body),
                    Connection {
                        peer,
                        tls: true,
                        listener: Listener {
                            ui: false,
                            webtransport: true,
                        },
                    },
                    &operations,
                    None,
                )
                .await?
            };
            send.response(response, head).await
        };
        let mut exchange = std::pin::pin!(exchange);
        let guarded = std::future::poll_fn(|cx| {
            check_operations(&operations, cx)?;
            let result = exchange.as_mut().poll(cx);
            if result.is_pending() {
                // Dispatch may install a lease in this poll. Register its wake
                // even when QUIC flow control blocks the first response write.
                check_operations(&operations, cx)?;
            }
            result
        });
        tokio::time::timeout(self.config.max_operation_duration, guarded)
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }
}

fn check_operations(operations: &Operations, cx: &mut Context<'_>) -> io::Result<()> {
    for operation in operations.lock().expect("operations poisoned").iter() {
        operation.lock().expect("operation poisoned").check(cx)?;
    }
    Ok(())
}

struct RequestBody {
    stream: Receive,
    finished: bool,
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        let result = match ready!(self.stream.poll_recv_data(cx)) {
            Ok(Some(mut data)) => Some(Ok(Frame::data(data.copy_to_bytes(data.remaining())))),
            Ok(None) => None,
            Err(error) => Some(Err(io::Error::other(error))),
        };
        if result.is_none() || result.as_ref().is_some_and(Result::is_err) {
            self.finished = true;
        }
        Poll::Ready(result)
    }

    fn is_end_stream(&self) -> bool {
        self.finished
    }
}

impl Drop for RequestBody {
    fn drop(&mut self) {
        if !self.finished {
            self.stream.stop_sending(Code::H3_REQUEST_CANCELLED);
        }
    }
}

struct ResponseStream {
    stream: Send,
    finished: bool,
}

impl ResponseStream {
    async fn response(&mut self, response: Response<ResponseBody>, head: bool) -> io::Result<()> {
        let (parts, mut body) = response.into_parts();
        self.stream
            .send_response(Response::from_parts(parts, ()))
            .await
            .map_err(io::Error::other)?;
        if !head {
            while let Some(frame) =
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            {
                if let Ok(mut data) = frame?.into_data() {
                    while !data.is_empty() {
                        let chunk = data.split_to(data.len().min(DATA_BYTES));
                        self.stream
                            .send_data(chunk)
                            .await
                            .map_err(io::Error::other)?;
                    }
                }
            }
        }
        // Keep both the response body and registry alive until h3 drains its
        // DATA framing into QUIC and queues FIN. Noq's send_window bounds the
        // remaining retransmission queue; h3 exposes no per-stream ACK future.
        self.stream.finish().await.map_err(io::Error::other)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        if !self.finished {
            self.stream.stop_stream(Code::H3_REQUEST_CANCELLED);
        }
    }
}
