//! HTTP upload adapters; aggregate timing and ownership remain in UploadStore.
use super::*;
use crate::{
    timeouts::PROGRESS_HEARTBEAT,
    upload::{Owner, UploadSubscription},
};
use futures_util::{Stream, stream};
use graphite_meter_core::{
    failure::UploadRefusal,
    wire::{UploadProgress, encode_upload_progress},
};
use http::HeaderValue;
use tokio::time::Instant;

impl HttpServer {
    /// A request's login or grant owns its uploads and admission; otherwise its client does. A trusted proxy's socket
    /// address is never the owner: without usable evidence, as in Go, nothing is.
    pub(super) fn owner<B>(&self, request: &Request<B>, lease: Option<&AuthLease>, peer: SocketAddr) -> Owner {
        if let Some(lease) = lease {
            return lease.owner();
        }
        let client = client_address::resolve(peer, request.headers(), &self.config.trusted_proxies);
        if client.usable { Owner::anonymous(client.addr) } else { Owner::unresolved() }
    }

    pub(super) fn upload_control(&self, route: Route, request: &Request<()>, owner: &Owner) -> Response<ResponseBody> {
        let id = query(request, "id").unwrap_or_default();
        match route {
            Route::UploadSession => match self.uploads.mint() {
                Some(id) => json_response(serde_json::json!({"uploadId": id}).to_string()),
                None => {
                    let mut response = text_body(StatusCode::SERVICE_UNAVAILABLE, "upload session mint failed");
                    response
                        .headers_mut()
                        .insert("x-graphite-upload-refusal", HeaderValue::from_static("unavailable"));
                    response
                }
            },
            Route::UploadCheckpoint => match self.uploads.checkpoint(&id, owner) {
                Ok(checkpoint) => json_response(serde_json::to_vec(&checkpoint).expect("serializable checkpoint")),
                Err(error) => refusal(error),
            },
            Route::UploadProgress => {
                if !matches!(*request.method(), Method::GET | Method::DELETE) {
                    return method_not_allowed("GET, DELETE");
                }
                let operation = match self.admit(Route::UploadProgress, owner) {
                    Ok(permit) => self.operation(Some(permit), false),
                    Err(refusal) => return *refusal,
                };
                let response = if request.method() == Method::DELETE {
                    match self.uploads.finish(&id, owner) {
                        Ok(()) => empty_response(StatusCode::NO_CONTENT),
                        Err(error) => refusal(error),
                    }
                } else {
                    let mut response = match self.uploads.subscribe(&id, owner) {
                        Ok(subscription) => Response::builder()
                            .header(header::CONTENT_TYPE, "application/x-ndjson")
                            .body(ResponseBody::progress(ProgressBody::new(subscription)))
                            .expect("static progress response"),
                        Err(error) => refusal(error),
                    };
                    let headers = response.headers_mut();
                    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, no-transform"));
                    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
                    response
                };
                with_operation(response, operation)
            }
            _ => text_response(StatusCode::NOT_FOUND),
        }
    }

    pub(super) async fn receive_upload<B>(
        &self,
        request: Request<B>,
        owner: &Owner,
        operations: &Operations,
    ) -> io::Result<Response<ResponseBody>>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let operation = match self.admit(Route::Upload, owner) {
            Ok(permit) => self.operation(Some(permit), false),
            Err(refusal) => return Ok(*refusal),
        };
        // Register before awaiting the body: socket IO must enforce this deadline
        // even while the response future has not produced its first byte.
        lock(operations).push(operation.clone());
        let mut lane = match self.uploads.begin(&query(&request, "id").unwrap_or_default(), owner) {
            Ok(lane) => lane,
            Err(error) => return Ok(with_operation(refusal(error), operation)),
        };
        let mut body = request.into_body();
        let deadline = lock(&operation).deadline.deadline();
        let mut idle = Instant::now() + IDLE_BOUND;
        loop {
            let frame = tokio::time::timeout_at(
                deadline.min(idle),
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)),
            )
            .await;
            let frame = match frame {
                Ok(Some(frame)) => frame.map_err(io::Error::other)?,
                Ok(None) => break,
                Err(_) if idle < deadline => return Ok(with_operation(refusal(UploadRefusal::Idle), operation)),
                Err(_) => return Err(io::ErrorKind::TimedOut.into()),
            };
            if let Ok(data) = frame.into_data() {
                lane.record(data.len());
                if !data.is_empty() {
                    idle = Instant::now() + IDLE_BOUND;
                }
            }
        }
        let bytes = lane.bytes();
        drop(lane);
        let response = json_response(serde_json::json!({"bytes": bytes}).to_string());
        Ok(with_operation(response, operation))
    }
}

fn with_operation(mut response: Response<ResponseBody>, operation: Arc<Mutex<Operation>>) -> Response<ResponseBody> {
    lock(&operation).body_complete = response.body().is_end_stream();
    response.body_mut().operation = Some(operation);
    response
}

type ProgressStream = Pin<Box<dyn Stream<Item = UploadProgress> + Send>>;

pub(super) struct ProgressBody {
    subscription: Option<ProgressStream>,
    heartbeat: Pin<Box<Sleep>>,
}

impl ProgressBody {
    fn new(subscription: UploadSubscription) -> Self {
        Self {
            subscription: Some(Box::pin(stream::unfold(subscription, |mut subscription| async move {
                subscription.next().await.map(|event| (event, subscription))
            }))),
            heartbeat: Box::pin(tokio::time::sleep(PROGRESS_HEARTBEAT)),
        }
    }

    pub(super) fn is_end_stream(&self) -> bool {
        self.subscription.is_none()
    }

    pub(super) fn poll_frame(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let Some(subscription) = self.subscription.as_mut() else {
            return Poll::Ready(None);
        };
        if let Poll::Ready(event) = subscription.as_mut().poll_next(cx) {
            let Some(event) = event else {
                self.subscription = None;
                return Poll::Ready(None);
            };
            if matches!(event, UploadProgress::Complete { .. }) {
                self.subscription = None;
            }
            let record = encode_upload_progress(&event)
                .map(|record| Frame::data(Bytes::from(format!("{record}\n"))))
                .map_err(io::Error::other);
            return Poll::Ready(Some(record));
        }
        if self.heartbeat.as_mut().poll(cx).is_ready() {
            self.heartbeat
                .as_mut()
                .reset(tokio::time::Instant::now() + PROGRESS_HEARTBEAT);
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"\n")))));
        }
        Poll::Pending
    }
}

pub(super) fn refusal(refusal: UploadRefusal) -> Response<ResponseBody> {
    let status = StatusCode::from_u16(refusal.status()).expect("known refusal status");
    let mut response = text_body(status, refusal.message());
    let headers = response.headers_mut();
    headers.insert("x-graphite-upload-refusal", HeaderValue::from_static(refusal.name()));
    if matches!(refusal, UploadRefusal::GlobalFull | UploadRefusal::ClientFull) {
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    if refusal == UploadRefusal::Revoked {
        headers.insert("graphite-meter-auth", HeaderValue::from_static("required"));
    }
    response
}
