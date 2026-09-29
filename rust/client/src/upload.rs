//! Stage-owned upload lanes and authoritative receiver evidence.
use crate::{
    Error,
    failure::Failure,
    transport::{REDIAL_WINDOW, Retrying, TRANSFER_RETRY_BACKOFF, TransferRetry, Transport, cache_buster, restore},
    webtransport::SessionSlot,
};
use bytes::Bytes;
use graphite_meter_core::{
    failure::UploadRefusal,
    measurement::{ObservedUpload, ReceiverSnapshot},
    route::Route,
    wire::{self, MAX_TRANSFER_BYTES, MAX_UPLOAD_COUNTER, MAX_WEBTRANSPORT_STREAMS, UploadProgress},
};
use graphite_meter_http3::webtransport::RecvStream;
use http::Method;
use serde::Deserialize;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::watch, task::JoinSet, time::Instant};

const FEED_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const CHECKPOINT_RETRY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug)]
struct ReceiverProgress {
    bytes: u64,
    nanos: u64,
}
#[derive(Clone, Default)]
struct State {
    ready: bool,
    complete: bool,
    latest: Option<ReceiverProgress>,
    error: Option<Arc<Error>>,
}

/// Dropping it aborts every owned task; finish(false) only sends its DELETE, finish(true) also awaits `complete`.
pub struct Upload {
    plan: Plan,
    id: String,
    state: watch::Receiver<State>,
    stop_lanes: watch::Sender<bool>,
    lanes: JoinSet<()>,
    progress: JoinSet<()>,
    session: Option<Arc<SessionSlot>>,
    retrying: Retrying,
}

/// What a receiver starts from, so that its replacement starts the same lanes on the same connections.
#[derive(Clone)]
struct Plan {
    transport: Arc<Transport>,
    control: Arc<Transport>,
    lanes: usize,
    /// HTTP lanes' stagger and request limit; WebTransport lanes take neither.
    http: Option<(Duration, Duration)>,
    cancel: watch::Receiver<bool>,
    /// Go's replacedUpload (upload.go:23-31): one replacement receiver per server and run.
    replaced: Arc<AtomicBool>,
}

impl Plan {
    /// Takes the server's one replacement when its receiver refused the upload id as `invalid`.
    fn replaces(&self, error: &Error) -> bool {
        crate::failure::refused(error.as_ref(), UploadRefusal::Invalid) && !self.replaced.swap(true, Ordering::Relaxed)
    }
}

impl Upload {
    /// `lanes` lanes over HTTP with `http`'s stagger and request limit, or over WebTransport
    /// without: the stage-owned cancellation covers the stagger, and each request lasts up to the
    /// stage's operation limit, as Go's lanes last the stage. `replaced` records the server's one
    /// replacement receiver in its run.
    pub async fn start(
        transport: Arc<Transport>,
        lanes: usize,
        http: Option<(Duration, Duration)>,
        replaced: Arc<AtomicBool>,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if http.is_none() && lanes > MAX_WEBTRANSPORT_STREAMS {
            return Err("WebTransport upload supports at most sixteen streams per session".into());
        }
        if !(1..=128).contains(&lanes) || http.is_some_and(|(stagger, _)| stagger > Duration::from_millis(75)) {
            return Err("invalid upload lane count or stagger".into());
        }
        // Lanes and control requests never share a connection, as Go gives upload lanes a
        // transport of their own: HTTP/3 control takes a new QUIC connection, and HTTP/1.1 and
        // HTTP/2 lanes take connections apart from control requests and download reads.
        let (transport, control) = if http.is_none() {
            (transport.clone(), transport)
        } else if transport.is_http3() {
            let mut stopped = cancel.clone();
            let control = tokio::select! {
                biased;
                _ = stopped.wait_for(|cancelled| *cancelled) => return Err("upload cancelled before startup".into()),
                control = transport.isolated_connection() => control?,
            };
            (transport, control)
        } else {
            (Arc::new(transport.for_upload_lanes()), transport)
        };
        let plan = Plan {
            transport,
            control,
            lanes,
            http,
            cancel,
            replaced,
        };
        match Self::begin(plan.clone()).await {
            Err(error) if plan.replaces(&error) => Self::begin(plan).await,
            started => started,
        }
    }
    /// Whether this receiver's failure takes the server's one replacement (upload.go:23-31).
    pub(crate) fn replaces(&self, error: &Error) -> bool {
        self.plan.replaces(error)
    }
    /// Ends this receiver and starts another on the same connections; its new id resumes the
    /// aggregate's evidence, as Go's replacement does.
    pub(crate) async fn replace(self) -> Result<Self, Error> {
        let plan = self.plan.clone();
        let _ = self.finish(false).await;
        Self::begin(plan).await
    }
    /// Mints a receiver and starts its progress feed and lanes; ready once the receiver counts.
    async fn begin(plan: Plan) -> Result<Self, Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Minted {
            upload_id: String,
        }
        let mut cancel = plan.cancel.clone();
        // Go's mint is tried again for 2 s (upload.go:33-39).
        let mint = || plan.control.json(Method::POST, Route::UploadSession, &[]);
        let minted: Minted = tokio::select! {
            biased;
            _ = cancel.wait_for(|cancelled| *cancelled) => return Err("upload cancelled before startup".into()),
            minted = restore("upload session", Instant::now() + REDIAL_WINDOW, mint) => minted?,
        };
        if minted.upload_id.is_empty()
            || minted.upload_id.len() > 8192
            || minted.upload_id.bytes().any(|byte| byte <= 32 || byte == 127)
        {
            return Err("server returned an invalid upload session ID".into());
        }
        let mut block = vec![0_u8; 64 * 1024];
        getrandom::fill(&mut block).map_err(|_| "secure randomness unavailable")?;
        let (state_tx, state) = watch::channel(State::default());
        let mut owner = Self {
            plan,
            id: minted.upload_id,
            state,
            stop_lanes: watch::Sender::new(false),
            lanes: JoinSet::new(),
            progress: JoinSet::new(),
            session: None,
            retrying: Retrying::default(),
        };
        match owner.run(&state_tx, Bytes::from(block)).await {
            Ok(()) => Ok(owner),
            Err(error) => {
                let _ = owner.finish(false).await;
                Err(error)
            }
        }
    }
    /// Starts the progress feed and the lanes, and waits until the receiver counts their bytes.
    async fn run(&mut self, state: &watch::Sender<State>, block: Bytes) -> Result<(), Error> {
        let mut cancel = self.plan.cancel.clone();
        if self.plan.http.is_none() {
            let query = [("id", self.id.as_str())];
            self.session = Some(Arc::new(tokio::select! {
                biased;
                _ = cancel.wait_for(|cancelled| *cancelled) => return Err("WebTransport upload cancelled during setup".into()),
                session = self.plan.transport.webtransport_slot(Route::WtUpload, &query) => session?,
            }));
        }
        let (transport, id, feed, session) = (
            self.plan.control.clone(),
            self.id.clone(),
            state.clone(),
            self.session.clone(),
        );
        self.progress.spawn(async move {
            if let Err(error) = progress_feed(&transport, &id, &feed, session).await {
                fail(&feed, error);
            }
        });
        let started: Vec<_> = (0..self.plan.lanes)
            .map(|index| self.spawn_lane(index, state, &block))
            .collect();
        tokio::time::timeout(CONTROL_TIMEOUT, async {
            loop {
                self.health()?;
                let state = self.state.borrow().clone();
                let receiver_observed = state.latest.is_some_and(|count| count.bytes > 0 && count.nanos > 0);
                let all_lanes_started = started.iter().all(|active| active.load(Ordering::Acquire));
                if state.ready && receiver_observed && all_lanes_started {
                    return Ok(());
                }
                tokio::select! {
                    biased;
                    _ = cancel.wait_for(|cancelled| *cancelled) => return Err("upload cancelled during startup".into()),
                    changed = self.state.changed() => changed.map_err(|_| "upload workers ended before receiver became ready")?,
                }
            }
        })
        .await?
    }
    /// Lane `index`, which ends with the lanes, the stage or the receiver; its flag rises as it sends.
    fn spawn_lane(&mut self, index: usize, state: &watch::Sender<State>, block: &Bytes) -> Arc<AtomicBool> {
        let active = Arc::new(AtomicBool::new(false));
        let (transport, id, state, block) = (
            self.plan.transport.clone(),
            self.id.clone(),
            state.clone(),
            block.clone(),
        );
        let (session, sending, (stagger, limit)) =
            (self.session.clone(), active.clone(), self.plan.http.unwrap_or_default());
        let (mut stop, mut cancelled, mut health) = (
            self.stop_lanes.subscribe(),
            self.plan.cancel.clone(),
            self.state.clone(),
        );
        let retry = TransferRetry::new(self.retrying.clone(), index);
        self.lanes.spawn(async move {
            tokio::select! {
                biased;
                _ = stop.wait_for(|stopped| *stopped) => {},
                _ = cancelled.wait_for(|cancelled| *cancelled) => {},
                _ = health.wait_for(|state| state.error.is_some() || state.complete) => {},
                result = async {
                    if index > 0 && !stagger.is_zero() {
                        tokio::time::sleep(stagger * index as u32).await;
                    }
                    match session {
                        Some(session) => send_wt_reconnecting(&session, block, sending, retry).await,
                        None => send_lane(&transport, &id, index, block, sending, retry, limit).await,
                    }
                } => if let Err(error) = result {
                    fail(&state, error);
                },
            }
        });
        active
    }
    pub fn observed(&self) -> Option<ObservedUpload> {
        self.state.borrow().latest.map(|value| ObservedUpload {
            id: self.id.clone(),
            maximum: value.bytes,
        })
    }
    pub(crate) fn retrying(&self) -> Option<Error> {
        self.retrying.failure()
    }
    pub fn health(&self) -> Result<(), Error> {
        let state = self.state.borrow();
        if let Some(error) = &state.error {
            return Err(Failure::Shared(error.clone()).into());
        }
        if state.complete {
            return Err("upload receiver completed before stage finish".into());
        }
        if self.state.has_changed().is_err() {
            return Err("upload workers ended unexpectedly".into());
        }
        Ok(())
    }
    pub async fn checkpoint(&self, budget: Duration) -> Result<ReceiverSnapshot, Error> {
        #[derive(Deserialize)]
        struct Count {
            bytes: u64,
            nanos: u64,
        }
        let deadline = Instant::now() + budget;
        loop {
            self.health()?;
            let response = tokio::time::timeout_at(
                deadline,
                self.plan
                    .control
                    .json::<Count>(Method::POST, Route::UploadCheckpoint, &[("id", &self.id)]),
            )
            .await;
            let count = match response {
                Ok(Ok(count)) => count,
                Ok(Err(error)) if crate::failure::sign_in(error.as_ref()).is_none() => {
                    if deadline.saturating_duration_since(Instant::now()) <= CHECKPOINT_RETRY {
                        return Err(error);
                    }
                    tokio::time::sleep(CHECKPOINT_RETRY).await;
                    continue;
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "upload receiver checkpoint timed out",
                    )
                    .into());
                }
            };
            if count.bytes > MAX_UPLOAD_COUNTER || count.nanos == 0 || count.nanos > MAX_UPLOAD_COUNTER {
                return Err(wire::WireError::InvalidReceiverCheckpoint.into());
            }
            self.health()?;
            return Ok(ReceiverSnapshot {
                id: self.id.clone(),
                bytes: count.bytes,
                nanos: count.nanos,
            });
        }
    }
    pub async fn finish(mut self, confirm: bool) -> Result<(), Error> {
        let bound = if confirm {
            CONTROL_TIMEOUT
        } else {
            Duration::from_secs(1)
        };
        let result = tokio::time::timeout(bound, async {
            let _ = self.stop_lanes.send(true);
            while let Some(result) = self.lanes.join_next().await {
                result?;
            }
            let mut response = self
                .plan
                .control
                .receive(
                    Method::DELETE,
                    Route::UploadProgress,
                    &[("id", &self.id)],
                    64 * 1024,
                    CONTROL_TIMEOUT,
                )
                .await?;
            while response.chunk().await?.is_some() {}
            let state = self
                .state
                .wait_for(|state| state.error.is_some() || state.complete || !confirm)
                .await
                .map_err(|_| "upload progress ended without complete")?;
            match &state.error {
                Some(error) => Err::<_, Error>(Failure::Shared(error.clone()).into()),
                None => Ok(()),
            }
        })
        .await;
        self.lanes.abort_all();
        self.progress.abort_all();
        while self.lanes.join_next().await.is_some() {}
        while self.progress.join_next().await.is_some() {}
        if let Some(session) = self.session.take()
            && let Ok(session) = Arc::try_unwrap(session)
        {
            let _ = tokio::time::timeout(Duration::from_secs(1), session.close()).await;
        }
        result?
    }
}

async fn send_lane(
    transport: &Transport,
    id: &str,
    index: usize,
    block: Bytes,
    active: Arc<AtomicBool>,
    mut retry: TransferRetry,
    limit: Duration,
) -> Result<(), Error> {
    let lane = index.to_string();
    loop {
        let started = Instant::now();
        let moved = Arc::new(AtomicBool::new(false));
        let body = futures_util::stream::unfold(
            (block.clone(), MAX_TRANSFER_BYTES, active.clone(), moved.clone()),
            |(block, remaining, active, moved)| async move {
                if remaining == 0 {
                    return None;
                }
                active.store(true, Ordering::Release);
                if remaining < MAX_TRANSFER_BYTES {
                    moved.store(true, Ordering::Relaxed);
                }
                let size = remaining.min(block.len() as u64) as usize;
                Some((
                    Ok::<_, Error>(block.slice(..size)),
                    (block, remaining - size as u64, active, moved),
                ))
            },
        );
        let query = [("cb", &*cache_buster()), ("id", id), ("lane", &*lane)];
        let result = transport
            .send(Route::Upload, &query, body, MAX_TRANSFER_BYTES, limit)
            .await;
        // A lane the receiver ended as idle ends its attempt normally; any other answer, busy
        // included, means the lane made no progress, whatever it sent (upload.go:118-128).
        let idle = |error: &Error| match error.downcast_ref() {
            Some(Failure::Http { status, refusal, .. }) => (*status, *refusal) == (408, Some(UploadRefusal::Idle)),
            _ => false,
        };
        let result = result.or_else(|error| if idle(&error) { Ok(()) } else { Err(error) });
        let answer = result.as_ref().err().and_then(|error| error.downcast_ref::<Failure>());
        let answered = matches!(answer, Some(Failure::Http { .. } | Failure::SignIn { .. }));
        retry
            .ended(result, started, moved.load(Ordering::Relaxed) && !answered)
            .await?;
    }
}
/// Go's followUploadFeed and attach (upload.go:284-297, 228-259): a feed that fails is reopened
/// within 2 s, after 500 ms if it failed at once.
async fn progress_loop(transport: &Transport, id: &str, state: &watch::Sender<State>) -> Result<(), Error> {
    let mut deadline = Instant::now() + REDIAL_WINDOW;
    loop {
        let mut feed = restore("upload progress", deadline, || Feed::open(transport, id, state)).await?;
        let opened = Instant::now();
        match feed.follow(state).await {
            Ok(()) => return Ok(()),
            Err(error) if crate::failure::permanent(error.as_ref()) => return Err(error),
            Err(_) => deadline = Instant::now() + REDIAL_WINDOW,
        }
        if opened.elapsed() < TRANSFER_RETRY_BACKOFF {
            tokio::time::sleep(TRANSFER_RETRY_BACKOFF).await;
        }
    }
}

/// Where the receiver's progress arrives: its HTTP feed, or the upload session's first server stream.
enum Source {
    Http(crate::transport::Body),
    WebTransport(RecvStream),
}

/// The receiver's progress, a record a line of at most 64 KiB over either source, as api/upload.md
/// bounds it.
struct Feed {
    source: Source,
    line: Vec<u8>,
    chunk: Bytes,
}

impl Feed {
    fn new(source: Source) -> Self {
        Self {
            source,
            line: Vec::new(),
            chunk: Bytes::new(),
        }
    }

    /// Go's openUploadFeed (upload.go:299-318): open once the receiver answers `ready`.
    async fn open(transport: &Transport, id: &str, state: &watch::Sender<State>) -> Result<Self, Error> {
        let body = transport
            .receive(
                Method::GET,
                Route::UploadProgress,
                &[("id", id)],
                u64::MAX,
                FEED_LIFETIME,
            )
            .await?;
        let mut feed = Self::new(Source::Http(body));
        loop {
            let event = feed.next().await?;
            let ready = matches!(event, UploadProgress::Ready);
            if apply_event(event, state)? || ready {
                return Ok(feed);
            }
        }
    }

    /// Go's read (upload.go:261-275): records until `complete`.
    async fn follow(&mut self, state: &watch::Sender<State>) -> Result<(), Error> {
        while !state.borrow().complete && !apply_event(self.next().await?, state)? {}
        Ok(())
    }

    /// The next record that decodes; the receiver sends at least one a second.
    async fn next(&mut self) -> Result<UploadProgress, Error> {
        loop {
            if self.chunk.is_empty() {
                self.chunk = match &mut self.source {
                    Source::Http(body) => tokio::time::timeout(CONTROL_TIMEOUT, body.chunk())
                        .await??
                        .ok_or("upload progress ended without complete")?,
                    Source::WebTransport(stream) => {
                        stream.read_chunk().await?.ok_or("upload progress stream closed")?
                    }
                };
            }
            let end = self.chunk.iter().position(|&byte| byte == b'\n');
            let count = end.map_or(self.chunk.len(), |end| end + 1);
            if count > 64 * 1024 - self.line.len() {
                return Err("upload progress line exceeds 64 KiB".into());
            }
            self.line.extend_from_slice(&self.chunk.split_to(count));
            if end.is_some()
                && let Ok(event) = wire::decode_upload_progress(&std::mem::take(&mut self.line))
            {
                return Ok(event);
            }
        }
    }
}
fn fail(state: &watch::Sender<State>, error: Error) {
    state.send_modify(|state| {
        if state.error.is_none() {
            state.error = Some(Arc::new(error));
        }
    });
}

async fn send_wt_lane(
    session: &crate::webtransport::Session,
    block: Bytes,
    active: &AtomicBool,
    moved: &mut bool,
) -> Result<Infallible, Error> {
    loop {
        let mut stream = session.open_uni().await?;
        let mut remaining = MAX_TRANSFER_BYTES;
        while remaining > 0 {
            let size = remaining.min(block.len() as u64) as usize;
            stream.write_chunk(block.slice(..size)).await?;
            remaining -= size as u64;
            active.store(true, Ordering::Release);
            *moved = true;
        }
        stream.finish()?;
    }
}

async fn send_wt_reconnecting(
    slot: &SessionSlot,
    block: Bytes,
    active: Arc<AtomicBool>,
    retry: TransferRetry,
) -> Result<(), Error> {
    let active = &*active;
    slot.lane(retry, move |session| {
        let block = block.clone();
        async move {
            let mut moved = false;
            let Err(error) = send_wt_lane(&session, block, active, &mut moved).await;
            (Err(error), moved)
        }
    })
    .await
}

async fn progress_feed(
    transport: &Transport,
    id: &str,
    state: &watch::Sender<State>,
    session: Option<Arc<SessionSlot>>,
) -> Result<(), Error> {
    if let Some(session) = session {
        let read = async {
            let mut feed = Feed::new(Source::WebTransport(session.current().await.accept_uni().await?));
            loop {
                let event = tokio::time::timeout(CONTROL_TIMEOUT, feed.next()).await??;
                if apply_event(event, state)? {
                    return Ok::<_, Error>(());
                }
            }
        };
        match read.await {
            Ok(()) => return Ok(()),
            Err(error) if crate::failure::permanent(error.as_ref()) => return Err(error),
            Err(_) => {}
        }
        // Reattach only the receiver's control feed. Payload lanes remain WT.
    }
    progress_loop(transport, id, state).await
}
fn apply_event(event: UploadProgress, state: &watch::Sender<State>) -> Result<bool, Error> {
    match event {
        UploadProgress::Ready => state.send_modify(|state| state.ready = true),
        UploadProgress::Error { code, .. } => {
            let refusal = UploadRefusal::from_name(&code);
            // A withdrawn grant asks for sign-in, as Go's uploadRefusal (failure.go:69-70).
            if refusal == Some(UploadRefusal::Revoked) {
                return Err(Box::new(Failure::SignIn {
                    origin: String::new(),
                    login_url: String::new(),
                }));
            }
            return Err(Box::new(Failure::Http {
                status: refusal.map_or(400, |refusal| refusal.status()),
                from: crate::failure::source(Route::UploadProgress.path()),
                retry_after: Duration::ZERO,
                refusal,
            }));
        }
        UploadProgress::Progress { bytes, nanos } | UploadProgress::Complete { bytes, nanos } => {
            let old = state.borrow().latest;
            if old.is_some_and(|old| bytes < old.bytes || nanos < old.nanos) {
                return Ok(false);
            }
            let count = ReceiverProgress { bytes, nanos };
            let complete = matches!(event, UploadProgress::Complete { .. });
            state.send_modify(|state| {
                state.latest = Some(count);
                state.complete = complete;
            });
            return Ok(complete);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stage's operation limit, the lifetime a stage gives its lanes' requests.
    const OPERATION_LIMIT: Duration = Duration::from_secs(60);
    /// HTTP lanes that start together and last the operation limit.
    const HTTP_LANES: Option<(Duration, Duration)> = Some((Duration::ZERO, OPERATION_LIMIT));
    use crate::transport::TRANSFER_RETRY_BACKOFF;
    use graphite_meter_core::discovery::Protocol;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    /// A client of `origin` over `protocol`, sharing none of its connections.
    async fn transport(origin: &str, protocol: Protocol) -> Result<Transport, Error> {
        Transport::connect(crate::net::Http::new(false)?, origin, protocol).await
    }

    #[tokio::test]
    async fn http_lane_retries_dropped_streaming_request() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let transport = transport(&origin, Protocol::Http1).await?;
        let active = Arc::new(AtomicBool::new(false));
        let lane = tokio::spawn(async move {
            send_lane(
                &transport,
                "upload-session",
                0,
                Bytes::from(vec![42; 64 * 1024]),
                active,
                TransferRetry::new(Retrying::default(), 0),
                OPERATION_LIMIT,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut first_closed = None;
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().await?;
                if attempt == 1 {
                    assert!(
                        first_closed.is_some_and(|closed: Instant| { closed.elapsed() >= TRANSFER_RETRY_BACKOFF }),
                        "a dropped upload request was retried without pacing"
                    );
                }
                let mut headers = Vec::new();
                let mut chunk = [0_u8; 4096];
                let body_start = loop {
                    let count = stream.read(&mut chunk).await?;
                    assert!(count > 0, "upload ended before request headers");
                    headers.extend_from_slice(&chunk[..count]);
                    if let Some(end) = headers.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break end + 4;
                    }
                    assert!(headers.len() <= 16 * 1024, "upload headers are too large");
                };
                assert!(headers.starts_with(b"POST /upload?cb="));
                let mut payload_bytes = headers.len() - body_start;
                while attempt == 0 && payload_bytes < 128 * 1024 {
                    let count = stream.read(&mut chunk).await?;
                    assert!(count > 0, "upload ended before the partial payload");
                    payload_bytes += count;
                }
                // The first connection drops after accepting payload bytes;
                // the second request proves recovery after a partial upload.
                drop(stream);
                if attempt == 0 {
                    first_closed = Some(Instant::now());
                }
            }
            Ok::<_, Error>(())
        })
        .await??;
        assert!(!lane.is_finished());
        lane.abort();
        let _ = lane.await;
        Ok(())
    }

    /// The progress feed of a WebTransport receiver that writes `records` on its first stream, and
    /// the state it left; nothing answers the HTTP feed a failing one falls back to.
    async fn webtransport_feed(records: &'static [u8]) -> Result<(Result<(), Error>, State), Error> {
        let _ = crate::crypto::provider().install_default();
        let (endpoint, origin) = crate::fixtures::h3_endpoint()?;
        let server = tokio::spawn(async move {
            let quic = endpoint.accept().await.ok_or("endpoint closed")?.await?;
            let mut connection = graphite_meter_http3::server::Connection::new(quic, None);
            let (_, stream) = connection.next().await?.ok_or("no CONNECT")?.resolve().await?;
            let feed = async {
                let session =
                    graphite_meter_http3::webtransport::Session::accept(stream, http::HeaderMap::new()).await?;
                let mut stream = session.open_uni().await?;
                stream.write_all(records).await?;
                std::future::pending::<Result<(), Error>>().await
            };
            let (fed, ()) = tokio::join!(feed, async { while let Ok(Some(_)) = connection.next().await {} });
            fed
        });
        let http = crate::net::Http::new(true)?;
        let slot = SessionSlot::dial(&http, format!("{origin}/wt/upload?id=test-session")).await?;
        let closed = format!("http://{}", TcpListener::bind("127.0.0.1:0").await?.local_addr()?);
        let control = Transport::connect(http, &closed, Protocol::Http1).await?;
        let (state, observed) = watch::channel(State::default());
        let fed = progress_feed(&control, "test-session", &state, Some(Arc::new(slot))).await;
        server.abort();
        let observed = observed.borrow().clone();
        Ok((fed, observed))
    }

    /// A WebTransport receiver that withdraws the grant with a `revoked` record asks for sign-in,
    /// as Go's uploadRefusal (failure.go:69-70), not a refusal that falls back to the HTTP feed.
    #[tokio::test]
    async fn a_revoked_record_asks_for_sign_in() -> Result<(), Error> {
        let (fed, _) = webtransport_feed(b"{\"type\":\"ready\"}\n{\"type\":\"error\",\"code\":\"revoked\"}\n").await?;
        let error = fed.err().ok_or("the feed completed")?;
        assert!(crate::failure::sign_in(error.as_ref()).is_some(), "{error}");
        Ok(())
    }

    /// A WebTransport record may take the 64 KiB api/upload.md allows every record, as over HTTP.
    #[tokio::test]
    async fn a_webtransport_record_takes_64_kib() -> Result<(), Error> {
        static RECORDS: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
            let note = "x".repeat(60 * 1024);
            format!("{{\"type\":\"ready\"}}\n{{\"type\":\"complete\",\"bytes\":7,\"nanos\":9,\"note\":\"{note}\"}}\n")
                .into_bytes()
        });
        let (fed, state) = webtransport_feed(&RECORDS).await?;
        fed?;
        assert!(state.complete);
        let latest = state.latest.ok_or("no count")?;
        assert_eq!((latest.bytes, latest.nanos), (7, 9));
        Ok(())
    }

    /// A lane the receiver ends as idle, 408 with its refusal code, ends that attempt as Go's does
    /// (upload.go:124-130): the next request goes out at once, not after a failure's pause.
    #[tokio::test]
    async fn an_idle_ending_starts_the_next_request_at_once() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let transport = transport(&origin, Protocol::Http1).await?;
        let block = Bytes::from(vec![42; 64 * 1024]);
        let active = Arc::new(AtomicBool::new(false));
        let retry = TransferRetry::new(Retrying::default(), 0);
        let lane = tokio::spawn(async move {
            send_lane(&transport, "upload-session", 0, block, active, retry, OPERATION_LIMIT).await
        });
        let requests = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut requests, mut held) = (Vec::new(), Vec::new());
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await?;
                requests.push(Instant::now());
                let mut received = 0;
                while received < 128 * 1024 {
                    received += stream.read(&mut [0_u8; 64 * 1024]).await?;
                }
                let idle =
                    "HTTP/1.1 408 Request Timeout\r\nX-Graphite-Upload-Refusal: idle\r\nContent-Length: 0\r\n\r\n";
                stream.write_all(idle.as_bytes()).await?;
                // Closing with the body still arriving resets the connection, and macOS then drops the
                // unread answer; a real receiver ends only an idle lane, with nothing in flight.
                held.push(stream);
            }
            Ok::<_, Error>(requests)
        })
        .await;
        lane.abort();
        let requests = requests??;
        assert!(requests[1] - requests[0] < TRANSFER_RETRY_BACKOFF, "{requests:?}");
        Ok(())
    }

    /// A lane the server answers busy once part of its body arrived has not progressed, as Go's
    /// uploadLane counts no answer but 200 and the idle ending (upload.go:118-128): it fails after
    /// 2 s, so its server leaves as busy rather than retrying for the whole stage.
    #[tokio::test]
    async fn a_lane_answered_busy_fails_after_2_s() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut received, mut chunk) = (0, [0_u8; 64 * 1024]);
                    while received < 128 * 1024 {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(count) => received += count,
                        }
                    }
                    let busy = "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n";
                    let _ = stream.write_all(busy.as_bytes()).await;
                    while stream.read(&mut chunk).await.is_ok_and(|count| count > 0) {}
                });
            }
        });
        let transport = transport(&origin, Protocol::Http1).await?;
        let (block, active) = (Bytes::from(vec![42; 64 * 1024]), Arc::new(AtomicBool::new(false)));
        let retry = TransferRetry::new(Retrying::default(), 0);
        let started = Instant::now();
        let lane = send_lane(&transport, "upload-session", 0, block, active, retry, OPERATION_LIMIT);
        let ended = tokio::time::timeout(Duration::from_secs(5), lane).await;
        server.abort();
        let error = ended?.err().ok_or("the lane ended")?;
        let reason = crate::failure::reason(error.as_ref(), false);
        assert_eq!(
            reason,
            graphite_meter_core::failure::FailureReason::ServerBusy,
            "{error}"
        );
        assert!(started.elapsed() >= REDIAL_WINDOW);
        Ok(())
    }

    /// An HTTP/3 upload the server answers early, then stops reading, returns that answer, read
    /// once its body can no longer be sent, as Go's round trip returns the response: here busy,
    /// with its refusal code, where the stopped stream read as a lost connection.
    #[tokio::test]
    async fn an_http3_upload_answered_early_returns_the_answer() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let (endpoint, origin) = crate::fixtures::h3_endpoint()?;
        let server = tokio::spawn(async move {
            let quic = endpoint.accept().await.ok_or("endpoint closed")?.await?;
            let mut connection = graphite_meter_http3::server::Connection::new(quic, None);
            let (request, stream) = connection.next().await?.ok_or("no request")?.resolve().await?;
            let (mut send, mut recv) = stream.split();
            let mut received = 0;
            while received < 128 * 1024 {
                received += recv.data().await?.ok_or("body ended")?.len();
            }
            // A request that lets caches keep its answer is refused as invalid instead.
            let no_store = request
                .headers()
                .get(http::header::CACHE_CONTROL)
                .is_some_and(|value| value == "no-store");
            let refusal = if no_store {
                UploadRefusal::GlobalFull
            } else {
                UploadRefusal::Invalid
            };
            let busy = http::Response::builder()
                .status(refusal.status())
                .header("x-graphite-upload-refusal", refusal.name());
            send.send_response(busy.body(())?).await?;
            send.finish().await?;
            recv.stop(graphite_meter_http3::Code::H3_NO_ERROR);
            while let Ok(Some(_)) = connection.next().await {}
            Ok::<_, Error>(())
        });
        let transport = Transport::connect(crate::net::Http::new(true)?, &origin, Protocol::Http3).await?;
        let block = Bytes::from(vec![42_u8; 64 * 1024]);
        let blocks = futures_util::stream::repeat_with(move || Ok::<_, Error>(block.clone()));
        let body = futures_util::StreamExt::take(blocks, 1024);
        let query = [("id", "upload-session"), ("lane", "0")];
        let sent = tokio::time::timeout(
            Duration::from_secs(5),
            transport.send(Route::Upload, &query, body, 64 * 1024 * 1024, OPERATION_LIMIT),
        )
        .await?;
        server.abort();
        let error = sent.err().ok_or("the upload completed")?;
        let answer = error.downcast_ref::<Failure>();
        assert!(
            matches!(
                answer,
                Some(Failure::Http {
                    status: 503,
                    refusal: Some(UploadRefusal::GlobalFull),
                    ..
                })
            ),
            "{error:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn checkpoint_retries_a_refusal_and_keeps_its_cause_past_the_budget() -> Result<(), Error> {
        use std::sync::atomic::AtomicUsize;
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let refusals = Arc::new(AtomicUsize::new(usize::MAX));
        let remaining = refusals.clone();
        let server = tokio::spawn(async move {
            let mut refused = Instant::now();
            loop {
                let (mut stream, _) = listener.accept().await?;
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).await?;
                assert!(request[..count].starts_with(b"POST /upload/checkpoint?id=test-session"));
                if remaining
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                    .is_ok()
                {
                    refused = Instant::now();
                    stream
                        .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                        .await?;
                    continue;
                }
                let body = br#"{"bytes":123,"nanos":456}"#;
                let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                stream.write_all(headers.as_bytes()).await?;
                stream.write_all(body).await?;
                return Ok::<_, Error>(refused);
            }
        });
        let transport = Arc::new(transport(&origin, Protocol::Http1).await?);
        let (state_sender, state) = watch::channel(State::default());
        let (stop_lanes, _) = watch::channel(false);
        let upload = Upload {
            plan: Plan {
                transport: transport.clone(),
                control: transport,
                lanes: 1,
                http: None,
                cancel: watch::channel(false).1,
                replaced: Arc::default(),
            },
            id: "test-session".into(),
            state,
            stop_lanes,
            lanes: JoinSet::new(),
            progress: JoinSet::new(),
            session: None,
            retrying: Retrying::default(),
        };
        // A budget ending during a request times out; the cause survives when it ends between attempts.
        let mut reasons = Vec::new();
        while reasons.len() < 3 && reasons.last() != Some(&graphite_meter_core::failure::FailureReason::ServerBusy) {
            let refused = upload.checkpoint(CHECKPOINT_RETRY).await.unwrap_err();
            reasons.push(crate::failure::reason(refused.as_ref(), false));
        }
        assert_eq!(
            reasons.last(),
            Some(&graphite_meter_core::failure::FailureReason::ServerBusy),
            "{reasons:?}"
        );
        refusals.store(1, Ordering::SeqCst);
        let snapshot = upload
            .checkpoint(graphite_meter_core::measurement::CHECKPOINT_BUDGET)
            .await?;
        let retried = Instant::now();
        assert_eq!((snapshot.bytes, snapshot.nanos), (123, 456));
        assert!(state_sender.borrow().latest.is_none());
        assert!(retried - server.await?? >= CHECKPOINT_RETRY);
        Ok(())
    }

    /// A connection that stops reading once armed, if it carried an upload lane.
    struct Gate {
        inner: tokio::net::TcpStream,
        lanes: Arc<AtomicBool>,
        armed: Arc<AtomicBool>,
    }
    impl tokio::io::AsyncRead for Gate {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.armed.load(Ordering::SeqCst) && self.lanes.load(Ordering::SeqCst) {
                return std::task::Poll::Pending;
            }
            std::pin::Pin::new(&mut self.inner).poll_read(context, buffer)
        }
    }
    impl tokio::io::AsyncWrite for Gate {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
            data: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.inner).poll_write(context, data)
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(context)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(context)
        }
    }

    /// Upload lanes keep their own HTTP/2 connection, as Go's upload transport: a checkpoint
    /// never queues behind their unsent bodies, here a peer that stops reading the lanes.
    #[tokio::test]
    async fn checkpoints_never_queue_behind_http2_upload_lanes() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let armed = Arc::new(AtomicBool::new(false));
        let (transport, server) = receiver(0, armed.clone(), Default::default()).await?;
        let (_stop, cancel) = watch::channel(false);
        let upload = tokio::time::timeout(
            Duration::from_secs(5),
            Upload::start(transport, 2, HTTP_LANES, Arc::default(), cancel),
        )
        .await??;
        armed.store(true, Ordering::SeqCst);
        let checkpoint = upload
            .checkpoint(graphite_meter_core::measurement::CHECKPOINT_BUDGET)
            .await;
        drop(upload);
        server.abort();
        let snapshot = checkpoint?;
        assert_eq!((snapshot.bytes, snapshot.nanos), (1, 1));
        Ok(())
    }

    /// A stop ends an HTTP/3 upload's start at once, also while its control connection dials, so
    /// the stopped stage closes within the controller's 5 s grace (controller.rs:19).
    #[tokio::test]
    async fn a_stop_ends_an_http3_start_while_its_control_connection_dials() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let (endpoint, origin) = crate::fixtures::h3_endpoint()?;
        // Only the lanes' connection completes its handshake; the control connection's never does.
        let server = tokio::spawn(async move {
            let _lanes = endpoint.accept().await.ok_or("closed")?.await?;
            std::future::pending::<Result<(), Error>>().await
        });
        let transport = Transport::connect(crate::net::Http::new(true)?, &origin, Protocol::Http3).await?;
        let (stop, cancel) = watch::channel(false);
        let started = Instant::now();
        let stop_soon = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            stop.send_replace(true);
        };
        let (start, _) = tokio::join!(
            Upload::start(Arc::new(transport), 1, HTTP_LANES, Arc::default(), cancel),
            stop_soon
        );
        server.abort();
        assert!(
            start.is_err() && started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        Ok(())
    }

    /// The mint is tried again as Go's restore tries it (upload.go:33-39), here once a busy
    /// answer's Retry-After has passed.
    #[tokio::test]
    async fn a_busy_mint_is_tried_again() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let requests = Requests::default();
        let (transport, server) = receiver(1, Default::default(), requests.clone()).await?;
        let (_stop, cancel) = watch::channel(false);
        let upload = Upload::start(transport, 1, HTTP_LANES, Arc::default(), cancel).await;
        server.abort();
        drop(upload?);
        let mints = times(&requests, "/upload/session");
        assert!(
            mints.len() == 2 && mints[1] - mints[0] >= Duration::from_secs(1),
            "{mints:?}"
        );
        Ok(())
    }

    /// An upload request lasts the stage's operation limit, as Go's lanes last their stage
    /// (upload.go:96-130), where a 120 s cap ended every lane at once.
    #[tokio::test(start_paused = true)]
    async fn an_upload_request_lasts_the_stage() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        // Paused time leaps to the next timer while a socket is awaited; this keeps leaps to 1 ms.
        let heartbeat = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        let requests = Requests::default();
        let (transport, server) = receiver(0, Default::default(), requests.clone()).await?;
        let (_stop, cancel) = watch::channel(false);
        let http = Some((Duration::ZERO, Duration::from_secs(300)));
        let upload = Upload::start(transport, 1, http, Arc::default(), cancel).await?;
        tokio::time::sleep(Duration::from_secs(200)).await;
        drop(upload);
        server.abort();
        heartbeat.abort();
        assert_eq!(times(&requests, "/upload").len(), 1);
        Ok(())
    }

    /// Each request's path and arrival.
    type Requests = Arc<std::sync::Mutex<Vec<(String, Instant)>>>;

    fn times(requests: &Requests, path: &str) -> Vec<Instant> {
        let requests = requests.lock().unwrap();
        requests
            .iter()
            .filter(|(seen, _)| seen == path)
            .map(|(_, at)| *at)
            .collect()
    }

    /// A receiver over HTTP/2 that answers its first `busy` mints with 503 and Retry-After: 1, and
    /// records each request; once `armed` it stops reading the connections that carried lanes.
    async fn receiver(
        busy: usize,
        armed: Arc<AtomicBool>,
        requests: Requests,
    ) -> Result<(Arc<Transport>, tokio::task::JoinHandle<()>), Error> {
        use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
        use hyper::{body::Frame, service::service_fn};
        use hyper_util::rt::{TokioExecutor, TokioIo};
        type Payload = BoxBody<Bytes, Infallible>;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let lanes = Arc::new(AtomicBool::new(false));
                let io = Gate {
                    inner: socket,
                    lanes: lanes.clone(),
                    armed: armed.clone(),
                };
                let requests = requests.clone();
                let service = service_fn(move |request: http::Request<hyper::body::Incoming>| {
                    let (lanes, requests) = (lanes.clone(), requests.clone());
                    async move {
                        let json =
                            |body: &'static str| -> Payload { Full::new(Bytes::from_static(body.as_bytes())).boxed() };
                        let path = request.uri().path();
                        requests.lock().unwrap().push((path.to_owned(), Instant::now()));
                        let mut answer = http::Response::builder();
                        let body = match path {
                            "/upload/session" => {
                                if times(&requests, path).len() <= busy {
                                    answer = answer.status(503).header(http::header::RETRY_AFTER, "1");
                                }
                                json(r#"{"uploadId":"fixture"}"#)
                            }
                            "/upload/checkpoint" => json(r#"{"bytes":1,"nanos":1}"#),
                            "/upload/progress" => {
                                let events = Bytes::from_static(
                                    b"{\"type\":\"ready\"}\n{\"type\":\"progress\",\"bytes\":1,\"nanos\":1}\n",
                                );
                                let chunks = futures_util::StreamExt::chain(
                                    futures_util::stream::once(async move { Ok(Frame::data(events)) }),
                                    futures_util::stream::pending(),
                                );
                                StreamBody::new(chunks).boxed()
                            }
                            "/upload" => {
                                lanes.store(true, Ordering::SeqCst);
                                std::future::pending::<()>().await;
                                unreachable!()
                            }
                            _ => json("{}"),
                        };
                        Ok::<_, Infallible>(answer.body(body).expect("fixture answer"))
                    }
                });
                tokio::spawn(
                    hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(io), service),
                );
            }
        });
        let transport = transport(&origin, Protocol::Http2).await?;
        Ok((Arc::new(transport), server))
    }

    #[tokio::test]
    async fn progress_feed_ignores_malformed_unknown_and_stale_records() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let count = stream.read(&mut [0_u8; 4096]).await?;
            assert!(count > 0);
            let body = concat!(
                "{\"type\":\"ready\"}\n",
                "{\"type\":\"progress\",\"bytes\":10,\"nanos\":20}\n",
                "not json\n\n",
                "{\"type\":\"future\"}\n",
                "{\"type\":\"progress\",\"bytes\":9,\"nanos\":21}\n",
                "{\"type\":\"complete\",\"bytes\":11,\"nanos\":19}\n",
                "{\"type\":\"complete\",\"bytes\":12,\"nanos\":30}\n",
            );
            let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
            stream.write_all(headers.as_bytes()).await?;
            stream.write_all(body.as_bytes()).await?;
            Ok::<_, Error>(())
        });
        let transport = transport(&origin, Protocol::Http1).await?;
        let (state, observed) = watch::channel(State::default());
        progress_loop(&transport, "test-session", &state).await?;
        server.await??;
        let observed = observed.borrow();
        assert!(observed.complete);
        let latest = observed.latest.unwrap();
        assert_eq!((latest.bytes, latest.nanos), (12, 30));
        Ok(())
    }

    /// A feed that cannot open is tried again for 2 s at Go's pace (upload.go:284-297,
    /// transfer.go:95-104): 500 ms after each refusal, where 100 ms made some twenty requests.
    #[tokio::test]
    async fn a_failing_progress_feed_reopens_at_gos_pace() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let opens = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = opens.clone();
        let server = tokio::spawn(async move {
            for _ in 0..32 {
                let (mut stream, _) = listener.accept().await?;
                seen.lock().unwrap().push(Instant::now());
                let _ = stream.read(&mut [0_u8; 4096]).await?;
                stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await?;
            }
            Ok::<_, Error>(())
        });
        let transport = transport(&origin, Protocol::Http1).await?;
        let (state, _) = watch::channel(State::default());
        let lost = progress_loop(&transport, "test-session", &state).await;
        server.abort();
        let opens = opens.lock().unwrap();
        assert!(lost.is_err_and(|error| error.to_string().contains("not replaced")));
        assert!((3..=5).contains(&opens.len()), "{} opens", opens.len());
        for pair in opens.windows(2) {
            assert!(pair[1] - pair[0] >= TRANSFER_RETRY_BACKOFF);
        }
        Ok(())
    }
}
