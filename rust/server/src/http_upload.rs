//! HTTP upload adapters; aggregate timing and ownership remain in UploadStore.
use super::*;
use crate::upload::{Owner, UploadError, UploadSubscription};
use graphite_meter_core::wire::{UploadProgress, encode_upload_progress};
use http::HeaderValue;
use serde::Serialize;

impl HttpServer {
    pub(super) fn upload_owner<B>(&self, request: &Request<B>, peer: SocketAddr) -> Owner {
        Owner::anonymous(
            client_address::resolve(peer, request.headers(), &self.config.trusted_proxies).addr,
        )
    }

    pub(super) fn upload_control(
        &self,
        request: &Request<()>,
        owner: &Owner,
    ) -> Response<ResponseBody> {
        let id = upload_id(request);
        match request.uri().path() {
            "/upload/session" => {
                if request.method() != Method::POST {
                    return empty_response(StatusCode::METHOD_NOT_ALLOWED);
                }
                match self.uploads.mint() {
                    Ok(id) => json_response(&serde_json::json!({"uploadId": id})),
                    Err(error) => refusal(error),
                }
            }
            "/upload/checkpoint" => {
                if request.method() != Method::POST {
                    return empty_response(StatusCode::METHOD_NOT_ALLOWED);
                }
                match self.uploads.checkpoint(&id, owner) {
                    Ok(checkpoint) => json_response(&checkpoint),
                    Err(error) => refusal(error),
                }
            }
            "/upload" | "/upload/progress" => {
                let operation = match self.upload_operation(owner) {
                    Ok(operation) => operation,
                    Err(error) => return admission_refusal(error),
                };
                let mut response = if request.uri().path() == "/upload" {
                    // The synchronous API represents an empty request body.
                    match self.uploads.begin(&id, owner) {
                        Ok(_lane) => json_response(&serde_json::json!({"bytes": 0})),
                        Err(error) => refusal(error),
                    }
                } else if request.method() == Method::DELETE {
                    match self.uploads.finish(&id, owner) {
                        Ok(()) => empty_response(StatusCode::NO_CONTENT),
                        Err(error) => refusal(error),
                    }
                } else if request.method() == Method::GET {
                    let mut response = match self.uploads.subscribe(&id, owner) {
                        Ok(subscription) => Response::builder()
                            .header(header::CONTENT_TYPE, "application/x-ndjson")
                            .body(ResponseBody {
                                block: Bytes::new(),
                                remaining: 0,
                                operation: None,
                                auth_operation: None,
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
                } else {
                    empty_response(StatusCode::METHOD_NOT_ALLOWED)
                };
                attach_operation(&mut response, operation);
                response
            }
            _ => text_response(StatusCode::NOT_FOUND),
        }
    }

    fn upload_operation(
        &self,
        owner: &Owner,
    ) -> Result<Arc<Mutex<Operation>>, crate::admission::Refusal> {
        self.admission
            .acquire(Class::Request, owner.budget_key())
            .map(|permit| {
                Arc::new(Mutex::new(Operation {
                    permit: Some(permit),
                    deadline: Box::pin(tokio::time::sleep(self.config.max_operation_duration)),
                    body_complete: false,
                    revocation: None,
                    revoked: false,
                }))
            })
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
        let operation = match self.upload_operation(owner) {
            Ok(operation) => operation,
            Err(error) => return Ok(admission_refusal(error)),
        };
        // Register before awaiting the body: socket IO must enforce this deadline
        // even while the response future has not produced its first byte.
        operations
            .lock()
            .expect("connection operations poisoned")
            .push(operation.clone());
        let mut lane = match self.uploads.begin(&upload_id(&request), owner) {
            Ok(lane) => lane,
            Err(error) => {
                let mut response = refusal(error);
                attach_operation(&mut response, operation);
                return Ok(response);
            }
        };
        let mut body = request.into_body();
        let receive = async {
            while let Some(frame) =
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            {
                let frame = frame.map_err(io::Error::other)?;
                if let Ok(data) = frame.into_data() {
                    lane.record(data.len());
                }
            }
            Ok::<_, io::Error>(lane.bytes())
        };
        let bytes = tokio::time::timeout(
            self.config
                .max_operation_duration
                .min(Duration::from_secs(120)),
            receive,
        )
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        drop(lane);
        let mut response = json_response(&serde_json::json!({"bytes": bytes}));
        attach_operation(&mut response, operation);
        Ok(response)
    }
}

fn upload_id<B>(request: &Request<B>) -> String {
    url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .find(|(key, _)| key == "id")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
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
            serde_json::to_vec(value)
                .expect("serializable upload document")
                .into(),
        ))
        .expect("static JSON response")
}

fn admission_refusal(error: crate::admission::Refusal) -> Response<ResponseBody> {
    let mut response = text_response(StatusCode::from_u16(error.status()).expect("known refusal"));
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

fn refusal(error: UploadError) -> Response<ResponseBody> {
    let mut response = text_response(error.status());
    *response.body_mut() = ResponseBody::bytes(Bytes::from(format!("{error}\n")));
    response.headers_mut().insert(
        "x-graphite-upload-refusal",
        HeaderValue::from_static(error.code()),
    );
    if error.retry() {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

type NextProgress =
    Pin<Box<dyn Future<Output = (UploadSubscription, Option<UploadProgress>)> + Send>>;

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

    pub(super) fn poll_frame(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Poll::Ready((subscription, event)) = self
            .next
            .as_mut()
            .expect("active progress future")
            .as_mut()
            .poll(cx)
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
