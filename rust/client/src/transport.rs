//! Streaming HTTP operations shared by measurement lanes and control requests.
//! The connection owns its H3 driver; response bodies retain that owner.
use crate::{
    Error,
    failure::Failure,
    net::{Http, url},
    quic::{Http3Client, Http3Stream},
    webtransport::SessionSlot,
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use graphite_meter_core::{
    discovery::Protocol, failure::FailureReason, origin::canonical_origin, route::Route, wire::decode_json,
};
use graphite_meter_http3::{self as http3, Code};
use http::{Method, Request, header::CACHE_CONTROL};
use serde::de::DeserializeOwned;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    runtime::Handle,
    sync::Mutex,
    time::{Instant, timeout_at},
};

/// Go's redialWindow: how long a lane or a dial may fail before it is lost.
pub(crate) const REDIAL_WINDOW: Duration = Duration::from_secs(2);
pub(crate) const TRANSFER_RETRY_BACKOFF: Duration = Duration::from_millis(500);
/// Go's busyBackoff and busyBackoffCap: a busy answer's first wait, doubled up to the cap.
const BUSY_BACKOFF: Duration = Duration::from_millis(300);
const BUSY_BACKOFF_CAP: Duration = Duration::from_millis(1200);

/// A thread for a connection and its lanes, as the server keeps its connections, in turn across the cores.
pub(crate) fn home() -> Handle {
    static POOL: OnceLock<Option<graphite_meter_net::Pool>> = OnceLock::new();
    let pool = POOL.get_or_init(|| graphite_meter_net::Pool::new().ok());
    pool.as_ref()
        .map_or_else(Handle::current, graphite_meter_net::Pool::next)
}

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
        retry_pause(started)
    }
}

/// Go's retryBackoff (transfer.go:100-103): an attempt that failed within it waits it out.
pub(crate) fn retry_pause(started: Instant) -> Duration {
    TRANSFER_RETRY_BACKOFF * u32::from(started.elapsed() < TRANSFER_RETRY_BACKOFF)
}

/// Go's restore (transfer.go:36-59): `attempt`, paced as a lane's retries, until done or `deadline`.
pub(crate) async fn restore<T, F: Future<Output = Result<T, Error>>>(
    what: &'static str,
    deadline: Instant,
    mut attempt: impl FnMut() -> F,
) -> Result<T, Error> {
    let (mut backoff, mut cause) = (RetryBackoff::default(), None);
    let window = deadline.saturating_duration_since(Instant::now());
    let lost = |error| -> Error { Failure::NotReplaced(what, window, error).into() };
    loop {
        let started = Instant::now();
        let error = match timeout_at(deadline, attempt()).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(error)) if crate::failure::permanent(error.as_ref()) => return Err(error),
            Ok(Err(error)) => error,
            // A try the deadline cut short keeps the cause before it.
            Err(elapsed) => return Err(lost(cause.unwrap_or_else(|| elapsed.into()))),
        };
        let wake = Instant::now() + backoff.delay(error.as_ref(), started);
        tokio::time::sleep_until(wake.min(deadline)).await;
        if wake >= deadline {
            return Err(lost(error));
        }
        cause = Some(error);
    }
}

#[derive(Clone, Default)]
pub(crate) struct Retrying(Arc<std::sync::Mutex<BTreeMap<usize, Arc<Error>>>>);

impl Retrying {
    pub(crate) fn failure(&self) -> Option<Error> {
        let lanes = self.0.lock().expect("retrying lanes poisoned");
        let error = lanes.values().next()?;
        Some(Failure::Shared(error.clone()).into())
    }
}

/// Ends a lane whose attempts fail without moving bytes; silence is the stage's rule.
#[derive(Default)]
pub(crate) struct TransferRetry {
    failing_since: Option<Instant>,
    backoff: RetryBackoff,
    retrying: Retrying,
    lane: usize,
}

impl TransferRetry {
    pub(crate) fn new(retrying: Retrying, lane: usize) -> Self {
        Self { retrying, lane, ..Self::default() }
    }

    /// Go's persist after an attempt (transfer.go:84-107): a clean attempt that moved bytes goes on
    /// at once, one that moved none stalls, and an error retries unless `failure::retryable` refuses.
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
    /// Where the HTTP/2 or HTTP/3 connection and its lanes run.
    home: Handle,
}

/// When a request lasting `duration` from now ends.
fn deadline(duration: Duration) -> Result<Instant, Error> {
    let deadline = Instant::now().checked_add(duration);
    Ok(deadline.ok_or("request duration is too large")?)
}

/// Dials on `home`, which then runs the connection's endpoint and drivers.
async fn dial_h3(home: &Handle, origin: &str, http: &Http) -> Result<Http3Client, Error> {
    let (uri, insecure) = (origin.parse()?, http.insecure);
    home.spawn(async move { Http3Client::connect(&uri, insecure, Duration::from_secs(10)).await })
        .await?
}

impl Transport {
    /// Download lanes' target, over HTTP/1.1 or HTTP/2 on connections the lanes dial on their own threads.
    pub(crate) fn download_lanes(self: &Arc<Self>) -> Arc<Self> {
        if self.h3.is_some() {
            return self.clone();
        }
        let (http, origin, protocol) = (self.http.for_download_lanes(), self.origin.clone(), self.protocol);
        Arc::new(Self { http, origin, protocol, h3: None, home: self.home.clone() })
    }

    /// Upload lanes' target and their control requests', on connections apart, as Go's upload transport.
    pub(crate) async fn upload_split(self: Arc<Self>) -> Result<(Arc<Self>, Arc<Self>), Error> {
        let (http, origin, protocol) = (self.http.clone(), self.origin.clone(), self.protocol);
        if self.h3.is_some() {
            return Ok((self, Arc::new(Self::connect(http, &origin, protocol).await?)));
        }
        let http = http.for_upload_lanes();
        Ok((Arc::new(Self { http, origin, protocol, h3: None, home: home() }), self))
    }

    /// Where a lane runs: with its HTTP/2 or HTTP/3 connection, or, over HTTP/1.1, on a thread in turn.
    pub(crate) fn lane_home(&self) -> Handle {
        match self.protocol {
            Protocol::Http2 | Protocol::Http3 => self.home.clone(),
            Protocol::Http1 | Protocol::Negotiated => home(),
        }
    }

    pub async fn connect(http: Http, origin: &str, protocol: Protocol) -> Result<Self, Error> {
        let origin = canonical_origin(origin)?;
        let home = home();
        let h3 = match protocol {
            Protocol::Http3 => Some(Mutex::new(Arc::new(dial_h3(&home, &origin, &http).await?))),
            _ => None,
        };
        Ok(Self { http, origin, protocol, h3, home })
    }

    pub async fn webtransport_slot(&self, route: Route, query: &[(&str, &str)]) -> Result<SessionSlot, Error> {
        SessionSlot::dial(&self.http, url(&self.origin, route, query)).await
    }

    /// `request` on the HTTP/3 connection, redialled once it closed or went away; None over TCP.
    async fn open_h3(&self, request: http::request::Builder, target: &str) -> Result<Option<Http3Stream>, Error> {
        let Some(slot) = &self.h3 else {
            return Ok(None);
        };
        let client = {
            let mut owner = slot.lock().await;
            if owner.is_closed() {
                *owner = Arc::new(dial_h3(&self.home, &self.origin, &self.http).await?);
            }
            owner.clone()
        };
        let mut request = request.uri(target).header(CACHE_CONTROL, "no-store").body(())?;
        self.http.authorize(target, request.headers_mut())?;
        Ok(Some(client.open(request).await?))
    }

    /// Ends an HTTP/3 request's body and checks the server's answer.
    async fn answer_h3(&self, stream: &mut Http3Stream, target: &str) -> Result<(), Error> {
        self.answered(stream.send.finish().await, stream, target).await?;
        let response = stream.recv.response().await?;
        self.http.check_status(target, response.status(), response.headers())
    }

    /// `sent`, or why the body could not be sent: the answer of a server that answered early, read
    /// for a second, as Go's round trip returns it; otherwise the send's error.
    async fn answered(
        &self,
        sent: Result<(), http3::Error>,
        stream: &mut Http3Stream,
        target: &str,
    ) -> Result<(), Error> {
        let Err(error) = sent else { return Ok(()) };
        let Ok(Ok(response)) = tokio::time::timeout(Duration::from_secs(1), stream.recv.response()).await else {
            return Err(error.into());
        };
        let refusal = self.http.check_status(target, response.status(), response.headers());
        Err(refusal.err().unwrap_or_else(|| error.into()))
    }

    pub async fn receive(
        &self,
        method: Method,
        route: Route,
        query: &[(&str, &str)],
        limit: u64,
        duration: Duration,
    ) -> Result<Body, Error> {
        let (target, deadline) = (url(&self.origin, route, query), deadline(duration)?);
        let inner = timeout_at(deadline, async {
            let request = Request::builder().method(method.clone());
            Ok::<_, Error>(match self.open_h3(request, &target).await? {
                Some(mut stream) => {
                    self.answer_h3(&mut stream, &target).await?;
                    BodyInner::H3(Box::new(stream))
                }
                None => BodyInner::Http(self.http.request(method, &target, self.protocol).await?),
            })
        })
        .await??;
        Ok(Body { inner, deadline, remaining: limit })
    }

    /// Streams a finite body; no sender counts escape, as uploads measure the receiver's counters.
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
        let (target, deadline) = (url(&self.origin, route, query), deadline(duration)?);
        let octets = |request: http::request::Builder| {
            let request = request.header(http::header::CONTENT_TYPE, "application/octet-stream");
            request.header(http::header::CONTENT_LENGTH, length)
        };
        timeout_at(deadline, async {
            let request = octets(Request::builder().method(Method::POST));
            let Some(mut stream) = self.open_h3(request, &target).await? else {
                let request = octets(self.http.builder(Method::POST, &target)?).body(crate::net::streaming(body))?;
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
                self.answered(stream.send.send_data(chunk).await, &mut stream, &target)
                    .await?;
            }
            if sent != length {
                return Err("request body shorter than content length".into());
            }
            self.answer_h3(&mut stream, &target).await?;
            let inner = BodyInner::H3(Box::new(stream));
            let mut response = Body { inner, deadline, remaining: 64 * 1024 };
            while response.raw_chunk().await?.is_some() {}
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
        let body = self.receive(method, route, query, 64 * 1024, Duration::from_secs(10));
        let mut body = body.await?;
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
        // quic-go reads a body cut short of its length as EOF, ending the download attempt.
        match self.raw_chunk().await {
            Err(error) if error.downcast_ref() == Some(&http3::Error::Protocol(Code::H3_MESSAGE_ERROR)) => Ok(None),
            chunk => chunk,
        }
    }

    /// Control replies remain strict; neither draining nor bounds require collecting their bytes.
    async fn raw_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let chunk = timeout_at(self.deadline, async {
            match &mut self.inner {
                BodyInner::Http(response) => loop {
                    let Some(frame) = http_body_util::BodyExt::frame(response.body_mut()).await else {
                        break Ok(None);
                    };
                    if let Ok(data) = frame?.into_data() {
                        break Ok(Some(data));
                    }
                },
                BodyInner::H3(stream) => Ok::<_, Error>(stream.recv.data().await?),
            }
        })
        .await??;
        if let Some(chunk) = &chunk {
            let remaining = self.remaining.checked_sub(chunk.len() as u64);
            self.remaining = remaining.ok_or("response exceeds byte limit")?;
        }
        Ok(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Request bounds live at the common HTTP boundary; an owned body keeps its H3 driver alive.
    #[tokio::test]
    async fn native_streaming_and_body_limits() -> Result<(), Error> {
        let (endpoint, origin) = crate::fixtures::h3_endpoint()?;
        let server = tokio::spawn(async move {
            let mut rejected = tokio::task::JoinSet::new();
            let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
            rejected.spawn(async move { incoming.await });
            let mut h3 = crate::fixtures::h3_connection(&endpoint).await?;
            // Echo, bounded echo, upload reply, oversized reply, truncated upload and download.
            for (method, bytes, declared) in [
                (Method::GET, 12, None),
                (Method::GET, 12, None),
                (Method::POST, 12, None),
                (Method::POST, 65537, None),
                (Method::POST, 1, Some(2)),
                (Method::GET, 1, Some(2)),
            ] {
                let (request, stream) = h3.next().await?.ok_or("missing request")?.resolve().await?;
                assert_eq!(request.method(), method);
                let (mut send, mut recv) = stream.split();
                let mut payload = Vec::new();
                while let Some(chunk) = recv.data().await? {
                    payload.extend_from_slice(&chunk);
                }
                let expected: &[u8] = if method == Method::POST { b"native upload" } else { b"" };
                assert_eq!(payload, expected);
                let mut response = http::Response::builder().status(200);
                if let Some(length) = declared {
                    response = response.header(http::header::CONTENT_LENGTH, length);
                }
                send.send_response(response.body(())?).await?;
                send.send_data(Bytes::from(vec![42; bytes])).await?;
                send.finish().await?;
            }
            let (_, stream) = h3.next().await?.ok_or("missing hanging request")?.resolve().await?;
            let (mut send, _) = stream.split();
            send.send_response(http::Response::new(())).await?;
            // Keep the response open until its retained owner closes the connection.
            let _ = h3.next().await;
            drop(send);
            Ok::<_, Error>(())
        });
        let refused = Transport::connect(Http::new(false)?, &origin, Protocol::Http3).await;
        let text = crate::failure::text(refused.map(drop).unwrap_err().as_ref());
        assert!(text.starts_with("Certificate not trusted: "), "{text}");
        let transport = Transport::connect(Http::new(true)?, &origin, Protocol::Http3).await?;
        let download =
            |limit, millis| transport.receive(Method::GET, Route::Download, &[], limit, Duration::from_millis(millis));
        for limit in [100, 3] {
            let result = download(limit, 5000).await?.chunk().await;
            if limit == 100 {
                assert_eq!(result?.ok_or("missing echo")?.len(), 12);
            } else {
                assert!(result.unwrap_err().to_string().contains("exceeds byte limit"));
            }
        }
        for expected in [None, Some("exceeds byte limit"), Some("truncated")] {
            let body =
                futures_util::stream::iter([Ok(Bytes::from_static(b"native ")), Ok(Bytes::from_static(b"upload"))]);
            let (length, limit) = (13, Duration::from_secs(5));
            let result = transport.send(Route::Upload, &[], body, length, limit).await;
            match expected {
                None => result?,
                Some("truncated") => assert_eq!(
                    result.unwrap_err().downcast_ref(),
                    Some(&http3::Error::Protocol(Code::H3_MESSAGE_ERROR))
                ),
                Some(text) => assert!(result.unwrap_err().to_string().contains(text)),
            }
        }
        let mut short = download(100, 5000).await?;
        assert_eq!(short.chunk().await?.ok_or("missing partial download")?.len(), 1);
        assert!(short.chunk().await?.is_none());
        drop(short);
        // Waiting for the connection slot spends the same request budget as reading its body.
        let slot = transport.h3.as_ref().ok_or("not HTTP/3")?.lock().await;
        let locked = download(100, 20).await.err().ok_or("lock escaped deadline")?;
        assert!(locked.is::<tokio::time::error::Elapsed>());
        drop(slot);
        let mut hanging = download(100, 100).await?;
        drop(transport);
        assert!(hanging.chunk().await.unwrap_err().is::<tokio::time::error::Elapsed>());
        drop(hanging);
        tokio::time::timeout(Duration::from_secs(5), server).await???;
        Ok(())
    }
}
