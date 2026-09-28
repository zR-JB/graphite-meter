//! Streaming HTTP operations shared by measurement lanes and control requests.
//! The connection owns its H3 driver; response bodies retain that owner.
use crate::{
    Error,
    failure::MeasurementFailure,
    net::Http,
    quic::{Http3Client, Http3Stream, RequestLimits},
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use graphite_meter_core::{
    discovery::Protocol, failure::FailureReason, origin::canonical_origin, route::Route, wire::decode_json,
};
use http::{Method, Request};
use serde::de::DeserializeOwned;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, timeout_at},
};

pub(crate) const TRANSFER_PROGRESS_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const TRANSFER_RETRY_BACKOFF: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(crate) struct RetryBackoff {
    busy: Duration,
}

impl RetryBackoff {
    pub(crate) fn delay(&mut self, mut error: &(dyn std::error::Error + 'static), started: Instant) -> Duration {
        loop {
            if let Some(http) = error.downcast_ref::<crate::failure::HttpFailure>()
                && matches!(http.status, 429 | 503)
            {
                self.busy = (self.busy * 2)
                    .max(Duration::from_millis(300))
                    .min(Duration::from_millis(1200));
                return self.busy.max(http.retry_after).min(Duration::from_millis(1200));
            }
            let Some(source) = error.source() else {
                break;
            };
            error = source;
        }
        self.busy = Duration::ZERO;
        if started.elapsed() < TRANSFER_RETRY_BACKOFF {
            TRANSFER_RETRY_BACKOFF
        } else {
            Duration::ZERO
        }
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
            .map(|error| Box::new(crate::failure::SharedFailure(error.clone())) as Error)
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
            Ok(()) => Box::new(MeasurementFailure(FailureReason::Timeout)),
            Err(error) => error,
        };
        let retryable = crate::failure::retryable(&error);
        self.retry(error, started, moved, retryable).await
    }

    pub(crate) async fn retry(
        &mut self,
        error: Error,
        started: Instant,
        moved: bool,
        retryable: bool,
    ) -> Result<(), Error> {
        if moved {
            self.failing_since = None;
        }
        if !retryable || !moved && self.failing_since.get_or_insert(started).elapsed() >= TRANSFER_PROGRESS_TIMEOUT {
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
    insecure: bool,
    h3: Option<Mutex<Arc<Http3Client>>>,
}

impl Transport {
    pub(crate) fn is_http3(&self) -> bool {
        self.h3.is_some()
    }

    pub(crate) async fn isolated_connection(&self) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(
            Self::connect(self.http.clone(), &self.origin, self.protocol, self.insecure).await?,
        ))
    }

    /// This HTTP/1.1 or HTTP/2 target for upload lanes, over connections of their own.
    pub(crate) fn for_upload_lanes(&self) -> Self {
        Self {
            http: self.http.for_upload_lanes(),
            origin: self.origin.clone(),
            protocol: self.protocol,
            insecure: self.insecure,
            h3: None,
        }
    }

    async fn h3_client(&self) -> Result<Option<Arc<Http3Client>>, Error> {
        let Some(slot) = &self.h3 else {
            return Ok(None);
        };
        let mut owner = slot.lock().await;
        if owner.is_closed() {
            *owner =
                Arc::new(Http3Client::connect(&self.origin.parse()?, self.insecure, Duration::from_secs(10)).await?);
        }
        Ok(Some(owner.clone()))
    }

    pub async fn connect(http: Http, origin: &str, protocol: Protocol, insecure: bool) -> Result<Self, Error> {
        let origin = canonical_origin(origin)?;
        let h3 = if protocol == Protocol::Http3 {
            Some(Mutex::new(Arc::new(
                Http3Client::connect(&origin.parse()?, insecure, Duration::from_secs(10)).await?,
            )))
        } else {
            None
        };
        Ok(Self {
            http,
            origin,
            protocol,
            insecure,
            h3,
        })
    }

    pub async fn webtransport_slot(
        &self,
        route: Route,
        query: &[(&str, &str)],
    ) -> Result<crate::webtransport::SessionSlot, Error> {
        crate::webtransport::SessionSlot::dial(&self.http, self.url(route, query), self.insecure).await
    }

    fn url(&self, route: Route, query: &[(&str, &str)]) -> String {
        let mut url = format!("{}{}", self.origin, route.path());
        if !query.is_empty() {
            url.push('?');
            url.push_str(
                &form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(query.iter().copied())
                    .finish(),
            );
        }
        url
    }

    pub async fn receive(
        &self,
        method: Method,
        route: Route,
        query: &[(&str, &str)],
        limit: u64,
        duration: Duration,
    ) -> Result<Body, Error> {
        let target = self.url(route, query);
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or("request duration is too large")?;
        let inner = timeout_at(deadline, async {
            if let Some(h3) = self.h3_client().await? {
                let mut request = Request::builder().method(method).uri(&target);
                if let Some(auth) = self.http.authorization(&target) {
                    request = request.header(http::header::AUTHORIZATION, auth);
                }
                let mut stream = h3
                    .open(
                        request.body(())?,
                        RequestLimits {
                            timeout: duration,
                            max_send_bytes: 0,
                            max_receive_bytes: limit,
                        },
                    )
                    .await?;
                stream.finish().await?;
                let response = stream.response().await?;
                self.http.check_status(&target, response.status(), response.headers())?;
                Ok::<_, Error>(BodyInner::H3(Box::new(stream)))
            } else {
                Ok(BodyInner::Http(
                    self.http.request(method, &target, self.protocol).await?,
                ))
            }
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
        let target = self.url(route, query);
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or("request duration is too large")?;
        timeout_at(deadline, async {
            if let Some(h3) = self.h3_client().await? {
                let mut request = Request::builder()
                    .method(Method::POST)
                    .uri(&target)
                    .header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .header(http::header::CONTENT_LENGTH, length);
                if let Some(auth) = self.http.authorization(&target) {
                    request = request.header(http::header::AUTHORIZATION, auth);
                }
                let mut request = h3
                    .open(
                        request.body(())?,
                        RequestLimits {
                            timeout: duration,
                            max_send_bytes: length,
                            max_receive_bytes: 64 * 1024,
                        },
                    )
                    .await?;
                futures_util::pin_mut!(body);
                let mut sent = 0_u64;
                while let Some(chunk) = body.next().await {
                    let chunk = chunk?;
                    sent = sent
                        .checked_add(chunk.len() as u64)
                        .filter(|sent| *sent <= length)
                        .ok_or("request body exceeds content length")?;
                    request.send_data(chunk).await?;
                }
                if sent != length {
                    return Err("request body shorter than content length".into());
                }
                request.finish().await?;
                let response = request.response().await?;
                self.http.check_status(&target, response.status(), response.headers())?;
                request.recv_body().await?;
            } else {
                let request = self
                    .http
                    .builder(Method::POST, &target)?
                    .header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .header(http::header::CONTENT_LENGTH, length)
                    .body(crate::net::streaming(body))?;
                let response = self.http.send(request, self.protocol).await?;
                crate::net::bounded_body(response).await?;
            }
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
                BodyInner::H3(stream) => stream.recv_data().await,
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
