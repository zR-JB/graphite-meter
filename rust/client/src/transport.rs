//! Streaming HTTP operations shared by measurement lanes and control requests.
//! The connection owns its H3 driver; response bodies retain that owner.
use crate::{
    Error,
    failure::Failure,
    net::{Http, url},
    quic::{Http3Client, Http3Stream, RequestLimits},
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use graphite_meter_core::{
    discovery::Protocol, failure::FailureReason, origin::canonical_origin, route::Route, wire::decode_json,
};
use graphite_meter_http3::{self as http3, Code};
use http::{Method, Request, header::CACHE_CONTROL};
use serde::de::DeserializeOwned;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, timeout_at},
};

/// Go's redialWindow: how long a lane or a dial may fail before it is lost.
pub(crate) const REDIAL_WINDOW: Duration = Duration::from_secs(2);
pub(crate) const TRANSFER_RETRY_BACKOFF: Duration = Duration::from_millis(500);
/// Go's busyBackoff and busyBackoffCap: a busy answer's first wait, doubled up to the cap.
const BUSY_BACKOFF: Duration = Duration::from_millis(300);
const BUSY_BACKOFF_CAP: Duration = Duration::from_millis(1200);

/// A lane request's cache buster, the time in nanoseconds, as Go's (download.go:51, upload.go:106).
pub(crate) fn cache_buster() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    now.unwrap_or_default().as_nanos().to_string()
}

#[derive(Default)]
pub(crate) struct RetryBackoff {
    busy: Duration,
}

impl RetryBackoff {
    pub(crate) fn delay(&mut self, error: &(dyn std::error::Error + 'static), started: Instant) -> Duration {
        if let Some(retry_after) = crate::failure::busy_wait(error) {
            self.busy = (self.busy * 2).clamp(BUSY_BACKOFF, BUSY_BACKOFF_CAP);
            return self.busy.max(retry_after).min(BUSY_BACKOFF_CAP);
        }
        self.busy = Duration::ZERO;
        if started.elapsed() < TRANSFER_RETRY_BACKOFF {
            TRANSFER_RETRY_BACKOFF
        } else {
            Duration::ZERO
        }
    }
}

/// Go's restore (transfer.go:36-59): `attempt` again, each try bounded by `deadline`, until it
/// succeeds or fails permanently, paced as a lane's retries; past the deadline the error names
/// what was lost and the last cause.
pub(crate) async fn restore<T, F: Future<Output = Result<T, Error>>>(
    what: &'static str,
    deadline: Instant,
    mut attempt: impl FnMut() -> F,
) -> Result<T, Error> {
    let mut backoff = RetryBackoff::default();
    let mut cause = None;
    loop {
        let started = Instant::now();
        let error = match timeout_at(deadline, attempt()).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(error)) if crate::failure::permanent(error.as_ref()) => return Err(error),
            Ok(Err(error)) => error,
            // A try the deadline cut short keeps the cause before it.
            Err(elapsed) => return Err(Failure::NotReplaced(what, cause.unwrap_or_else(|| elapsed.into())).into()),
        };
        let wake = Instant::now() + backoff.delay(error.as_ref(), started);
        tokio::time::sleep_until(wake.min(deadline)).await;
        if wake >= deadline {
            return Err(Failure::NotReplaced(what, error).into());
        }
        cause = Some(error);
    }
}

#[derive(Clone, Default)]
pub(crate) struct Retrying(Arc<std::sync::Mutex<BTreeMap<usize, Arc<Error>>>>);

impl Retrying {
    pub(crate) fn failure(&self) -> Option<Error> {
        let lanes = self.0.lock().expect("retrying lanes poisoned");
        lanes
            .values()
            .next()
            .map(|error| Box::new(Failure::Shared(error.clone())) as Error)
    }
}

/// Ends a lane whose attempts fail without moving bytes; silence is the stage's rule.
pub(crate) struct TransferRetry {
    failing_since: Option<Instant>,
    backoff: RetryBackoff,
    retrying: Retrying,
    lane: usize,
}

impl TransferRetry {
    pub(crate) fn new(retrying: Retrying, lane: usize) -> Self {
        Self {
            failing_since: None,
            backoff: RetryBackoff::default(),
            retrying,
            lane,
        }
    }

    /// Go's persist after an attempt (transfer.go:84-107): an attempt that ended cleanly after
    /// moving bytes goes on at once; one that moved none counts as stalled; an error retries unless
    /// it is permanent (failure::retryable).
    pub(crate) async fn ended(
        &mut self,
        result: Result<(), Error>,
        started: Instant,
        moved: bool,
    ) -> Result<(), Error> {
        let error = match result {
            Ok(()) if moved => {
                self.failing_since = None;
                self.backoff = RetryBackoff::default();
                self.publish(None);
                return Ok(());
            }
            Ok(()) => Box::new(Failure::Measurement(FailureReason::Timeout)),
            Err(error) => error,
        };
        if moved {
            self.failing_since = None;
        }
        if !crate::failure::retryable(&error)
            || !moved && self.failing_since.get_or_insert(started).elapsed() >= REDIAL_WINDOW
        {
            return Err(error);
        }
        let delay = self.backoff.delay(error.as_ref(), started);
        self.publish((!moved).then(|| Arc::new(error)));
        tokio::time::sleep(delay).await;
        Ok(())
    }

    fn publish(&self, failure: Option<Arc<Error>>) {
        let mut lanes = self.retrying.0.lock().expect("retrying lanes poisoned");
        match failure {
            Some(failure) => lanes.insert(self.lane, failure),
            None => lanes.remove(&self.lane),
        };
    }
}

pub struct Transport {
    http: Http,
    origin: String,
    protocol: Protocol,
    h3: Option<Mutex<Arc<Http3Client>>>,
}

async fn dial_h3(origin: &str, http: &Http) -> Result<Http3Client, Error> {
    Http3Client::connect(&origin.parse()?, http.insecure, Duration::from_secs(10)).await
}

impl Transport {
    pub(crate) fn is_http3(&self) -> bool {
        self.h3.is_some()
    }

    pub(crate) async fn isolated_connection(&self) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(
            Self::connect(self.http.clone(), &self.origin, self.protocol).await?,
        ))
    }

    /// This HTTP/1.1 or HTTP/2 target for upload lanes, over connections of their own.
    pub(crate) fn for_upload_lanes(&self) -> Self {
        Self {
            http: self.http.for_upload_lanes(),
            origin: self.origin.clone(),
            protocol: self.protocol,
            h3: None,
        }
    }

    pub async fn connect(http: Http, origin: &str, protocol: Protocol) -> Result<Self, Error> {
        let origin = canonical_origin(origin)?;
        let h3 = match protocol {
            Protocol::Http3 => Some(Mutex::new(Arc::new(dial_h3(&origin, &http).await?))),
            _ => None,
        };
        Ok(Self {
            http,
            origin,
            protocol,
            h3,
        })
    }

    pub async fn webtransport_slot(
        &self,
        route: Route,
        query: &[(&str, &str)],
    ) -> Result<crate::webtransport::SessionSlot, Error> {
        crate::webtransport::SessionSlot::dial(&self.http, url(&self.origin, route, query)).await
    }

    /// `request` to `target` on this target's HTTP/3 connection, dialled again once it closed;
    /// None over HTTP/1.1 and HTTP/2, whose requests the client's pool carries.
    async fn open_h3(
        &self,
        request: http::request::Builder,
        target: &str,
        limits: RequestLimits,
    ) -> Result<Option<Http3Stream>, Error> {
        let Some(slot) = &self.h3 else {
            return Ok(None);
        };
        let client = {
            let mut owner = slot.lock().await;
            if owner.is_closed() {
                *owner = Arc::new(dial_h3(&self.origin, &self.http).await?);
            }
            owner.clone()
        };
        let mut request = request.uri(target).header(CACHE_CONTROL, "no-store").body(())?;
        self.http.authorize(target, request.headers_mut())?;
        Ok(Some(client.open(request, limits).await?))
    }

    /// Ends an HTTP/3 request's body and checks the server's answer.
    async fn answer_h3(&self, stream: &mut Http3Stream, target: &str) -> Result<(), Error> {
        if let Err(error) = stream.finish().await {
            return Err(self.early_answer(stream, target, error).await);
        }
        let response = stream.response().await?;
        self.http.check_status(target, response.status(), response.headers())
    }

    /// Why an HTTP/3 request's body could not be sent: a server that answers early stops reading
    /// it, so its answer, read for a second, names the refusal, as Go's round trip returns it;
    /// otherwise `error`.
    async fn early_answer(&self, stream: &mut Http3Stream, target: &str, error: Error) -> Error {
        match tokio::time::timeout(Duration::from_secs(1), stream.response()).await {
            Ok(Ok(response)) => self
                .http
                .check_status(target, response.status(), response.headers())
                .err()
                .unwrap_or(error),
            _ => error,
        }
    }

    pub async fn receive(
        &self,
        method: Method,
        route: Route,
        query: &[(&str, &str)],
        limit: u64,
        duration: Duration,
    ) -> Result<Body, Error> {
        let target = url(&self.origin, route, query);
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or("request duration is too large")?;
        let limits = RequestLimits {
            timeout: duration,
            max_send_bytes: 0,
            max_receive_bytes: limit,
        };
        let inner = timeout_at(deadline, async {
            let request = Request::builder().method(method.clone());
            Ok::<_, Error>(match self.open_h3(request, &target, limits).await? {
                Some(mut stream) => {
                    self.answer_h3(&mut stream, &target).await?;
                    BodyInner::H3(Box::new(stream))
                }
                None => BodyInner::Http(self.http.request(method, &target, self.protocol).await?),
            })
        })
        .await??;
        Ok(Body {
            inner,
            deadline,
            remaining: limit,
        })
    }

    /// Stream a finite request without materializing its body. No sender counts
    /// escape this API: upload measurements must use the receiver's counters.
    pub async fn send<S>(
        &self,
        route: Route,
        query: &[(&str, &str)],
        body: S,
        length: u64,
        duration: Duration,
    ) -> Result<(), Error>
    where
        S: Stream<Item = Result<Bytes, Error>> + Send + 'static,
    {
        let target = url(&self.origin, route, query);
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or("request duration is too large")?;
        let limits = RequestLimits {
            timeout: duration,
            max_send_bytes: length,
            max_receive_bytes: 64 * 1024,
        };
        timeout_at(deadline, async {
            let request = Request::builder()
                .method(Method::POST)
                .header(http::header::CONTENT_TYPE, "application/octet-stream")
                .header(http::header::CONTENT_LENGTH, length);
            let Some(mut stream) = self.open_h3(request, &target, limits).await? else {
                let request = self
                    .http
                    .builder(Method::POST, &target)?
                    .header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .header(http::header::CONTENT_LENGTH, length)
                    .body(crate::net::streaming(body))?;
                let response = self.http.send(request, self.protocol, None).await?;
                crate::net::bounded_body(response).await?;
                return Ok(());
            };
            futures_util::pin_mut!(body);
            let mut sent = 0_u64;
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                sent = sent
                    .checked_add(chunk.len() as u64)
                    .filter(|sent| *sent <= length)
                    .ok_or("request body exceeds content length")?;
                if let Err(error) = stream.send_data(chunk).await {
                    return Err(self.early_answer(&mut stream, &target, error).await);
                }
            }
            if sent != length {
                return Err("request body shorter than content length".into());
            }
            self.answer_h3(&mut stream, &target).await?;
            stream.recv_body().await?;
            Ok(())
        })
        .await?
    }

    pub async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        route: Route,
        query: &[(&str, &str)],
    ) -> Result<T, Error> {
        let mut body = self
            .receive(method, route, query, 64 * 1024, Duration::from_secs(10))
            .await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.chunk().await? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(decode_json(&bytes)?)
    }
}

enum BodyInner {
    Http(crate::net::Response),
    H3(Box<Http3Stream>),
}

pub struct Body {
    inner: BodyInner,
    deadline: Instant,
    remaining: u64,
}

impl Body {
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let chunk = timeout_at(self.deadline, async {
            match &mut self.inner {
                BodyInner::Http(response) => loop {
                    match http_body_util::BodyExt::frame(response.body_mut()).await {
                        None => break Ok(None),
                        Some(frame) => {
                            if let Ok(data) = frame?.into_data() {
                                break Ok(Some(data));
                            }
                        }
                    }
                },
                // quic-go reads a body cut short of its length as EOF (http3/body.go), which ends a
                // download attempt in Go (download.go:74) rather than the lane.
                BodyInner::H3(stream) => match stream.recv_data().await {
                    Err(error) if error.downcast_ref() == Some(&http3::Error::Protocol(Code::H3_MESSAGE_ERROR)) => {
                        Ok(None)
                    }
                    data => data,
                },
            }
        })
        .await??;
        if let Some(chunk) = &chunk {
            self.remaining = self
                .remaining
                .checked_sub(chunk.len() as u64)
                .ok_or("response exceeds byte limit")?;
        }
        Ok(chunk)
    }
}
