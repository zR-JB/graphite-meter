//! HTTP upload adapters; aggregate timing and ownership remain in UploadStore.
use super::*;
use crate::upload::{Owner, UploadSubscription};
use graphite_meter_core::{
    failure::UploadRefusal,
    wire::{UploadProgress, encode_upload_progress},
};
use http::HeaderValue;
use serde::Serialize;
use tokio::time::Instant;

impl HttpServer {
    pub(super) fn upload_owner<B>(&self, request: &Request<B>, peer: SocketAddr) -> Owner {
        Owner::anonymous(client_address::resolve(peer, request.headers(), &self.config.trusted_proxies).addr)
    }

    pub(super) fn upload_control(&self, request: &Request<()>, owner: &Owner) -> Response<ResponseBody> {
        let id = query(request, "id").unwrap_or_default();
        match request.uri().path() {
            "/upload/session" => match self.uploads.mint() {
                Some(id) => json_response(&serde_json::json!({"uploadId": id})),
                None => {
                    let mut response = text_body(StatusCode::SERVICE_UNAVAILABLE, "upload session mint failed");
                    response
                        .headers_mut()
                        .insert("x-graphite-upload-refusal", HeaderValue::from_static("unavailable"));
                    response
                }
            },
            "/upload/checkpoint" => match self.uploads.checkpoint(&id, owner) {
                Ok(checkpoint) => json_response(&checkpoint),
                Err(error) => refusal(error),
            },
            "/upload/progress" => {
                if !matches!(*request.method(), Method::GET | Method::DELETE) {
                    return method_not_allowed("GET, DELETE");
                }
                let operation = match self.admit(Route::UploadProgress, owner) {
                    Ok(permit) => self.operation(Some(permit), false),
                    Err(refusal) => return *refusal,
                };
                let mut response = if request.method() == Method::DELETE {
                    match self.uploads.finish(&id, owner) {
                        Ok(()) => empty_response(StatusCode::NO_CONTENT),
                        Err(error) => refusal(error),
                    }
                } else {
                    let mut response = match self.uploads.subscribe(&id, owner) {
                        Ok(subscription) => Response::builder()
                            .header(header::CONTENT_TYPE, "application/x-ndjson")
                            .body(ResponseBody {
                                block: Bytes::new(),
                                remaining: 0,
                                operation: None,
                                transfer: None,
                                progress: Some(ProgressBody::new(subscription)),
                            })
                            .expect("static progress response"),
                        Err(error) => refusal(error),
                    };
                    response.headers_mut().insert(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("no-store, no-transform"),
                    );
                    response
                        .headers_mut()
                        .insert("x-accel-buffering", HeaderValue::from_static("no"));
                    response
                };
                attach_operation(&mut response, operation);
                response
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
        operations
            .lock()
            .expect("connection operations poisoned")
            .push(operation.clone());
        let mut lane = match self.uploads.begin(&query(&request, "id").unwrap_or_default(), owner) {
            Ok(lane) => lane,
            Err(error) => {
                let mut response = refusal(error);
                attach_operation(&mut response, operation);
                return Ok(response);
            }
        };
        let mut body = request.into_body();
        let deadline = operation.lock().expect("operation poisoned").deadline.deadline();
        let mut idle = Instant::now() + Duration::from_secs(30);
        loop {
            let frame = tokio::time::timeout_at(
                deadline.min(idle),
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)),
            )
            .await;
            let frame = match frame {
                Ok(Some(frame)) => frame.map_err(io::Error::other)?,
                Ok(None) => break,
                Err(_) if idle < deadline => {
                    let mut response = refusal(UploadRefusal::Idle);
                    attach_operation(&mut response, operation);
                    return Ok(response);
                }
                Err(_) => return Err(io::ErrorKind::TimedOut.into()),
            };
            if let Ok(data) = frame.into_data() {
                lane.record(data.len());
                if !data.is_empty() {
                    idle = Instant::now() + Duration::from_secs(30);
                }
            }
        }
        let bytes = lane.bytes();
        drop(lane);
        let mut response = json_response(&serde_json::json!({"bytes": bytes}));
        attach_operation(&mut response, operation);
        Ok(response)
    }
}

fn attach_operation(response: &mut Response<ResponseBody>, operation: Arc<Mutex<Operation>>) {
    operation.lock().expect("operation poisoned").body_complete = response.body().is_end_stream();
    response.body_mut().operation = Some(operation);
}

fn empty_response(status: StatusCode) -> Response<ResponseBody> {
    Response::builder()
        .status(status)
        .body(ResponseBody::empty())
        .expect("static response")
}

fn json_response(value: &impl Serialize) -> Response<ResponseBody> {
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(ResponseBody::bytes(
            serde_json::to_vec(value).expect("serializable upload document").into(),
        ))
        .expect("static JSON response")
}

type NextProgress = Pin<Box<dyn Future<Output = (UploadSubscription, Option<UploadProgress>)> + Send>>;

pub(super) struct ProgressBody {
    next: Option<NextProgress>,
    heartbeat: Pin<Box<Sleep>>,
    pub(super) done: bool,
}

impl ProgressBody {
    fn new(subscription: UploadSubscription) -> Self {
        Self {
            next: Some(next_progress(subscription)),
            heartbeat: Box::pin(tokio::time::sleep(Duration::from_secs(1))),
            done: false,
        }
    }

    pub(super) fn poll_frame(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Poll::Ready((subscription, event)) =
            self.next.as_mut().expect("active progress future").as_mut().poll(cx)
        {
            self.next = None;
            let Some(event) = event else {
                self.done = true;
                return Poll::Ready(None);
            };
            self.done = matches!(event, UploadProgress::Complete { .. });
            if !self.done {
                self.next = Some(next_progress(subscription));
            }
            let record = encode_upload_progress(&event)
                .map(|record| Frame::data(Bytes::from(format!("{record}\n"))))
                .map_err(io::Error::other);
            return Poll::Ready(Some(record));
        }
        if self.heartbeat.as_mut().poll(cx).is_ready() {
            self.heartbeat
                .as_mut()
                .reset(tokio::time::Instant::now() + Duration::from_secs(1));
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"\n")))));
        }
        Poll::Pending
    }
}

fn next_progress(mut subscription: UploadSubscription) -> NextProgress {
    Box::pin(async move {
        let event = subscription.next().await;
        (subscription, event)
    })
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
