//! Stage-owned upload lanes and authoritative receiver evidence.
use crate::{
    Error,
    failure::SharedFailure,
    transport::{REDIAL_WINDOW, Retrying, TRANSFER_RETRY_BACKOFF, TransferRetry, Transport, restore},
    webtransport::{ConnectRejected, SessionSlot},
};
use bytes::Bytes;
use graphite_meter_core::{
    measurement::{ObservedUpload, ReceiverSnapshot},
    route::Route,
    wire::{self, MAX_TRANSFER_BYTES, MAX_UPLOAD_COUNTER, MAX_WEBTRANSPORT_STREAMS, UploadProgress},
};
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

const REQUEST_LIFETIME: Duration = Duration::from_secs(120);
const FEED_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LINE: usize = 64 * 1024;
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

/// Drop cancels every owned task; finish(false) only sends its DELETE, finish(true) also awaits `complete`.
pub struct Upload {
    transport: Arc<Transport>,
    control: Arc<Transport>,
    id: String,
    state: watch::Receiver<State>,
    stop_lanes: watch::Sender<bool>,
    stop_all: watch::Sender<bool>,
    lanes: JoinSet<()>,
    progress: JoinSet<()>,
    session: Option<Arc<SessionSlot>>,
    retrying: Retrying,
}

impl Upload {
    /// Stagger first HTTP requests inside the stage-owned cancellation scope.
    pub async fn start(
        transport: Arc<Transport>,
        lanes: usize,
        stagger: Duration,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        Self::start_inner(transport, lanes, cancel, false, stagger).await
    }
    pub async fn start_webtransport(
        transport: Arc<Transport>,
        lanes: usize,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if lanes > MAX_WEBTRANSPORT_STREAMS {
            return Err("WebTransport upload supports at most sixteen streams per session".into());
        }
        Self::start_inner(transport, lanes, cancel, true, Duration::ZERO).await
    }
    async fn start_inner(
        transport: Arc<Transport>,
        lanes: usize,
        mut cancel: watch::Receiver<bool>,
        webtransport: bool,
        stagger: Duration,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) || stagger > Duration::from_millis(75) {
            return Err("invalid upload lane count or stagger".into());
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Minted {
            upload_id: String,
        }
        // Lanes and control requests never share a connection, as Go gives upload lanes a
        // transport of their own: HTTP/3 control takes a new QUIC connection, and HTTP/1.1 and
        // HTTP/2 lanes take connections apart from control requests and download reads.
        let (transport, control) = if webtransport {
            (transport.clone(), transport)
        } else if transport.is_http3() {
            let control = transport.isolated_connection().await?;
            (transport, control)
        } else {
            (Arc::new(transport.for_upload_lanes()), transport)
        };
        let minted: Minted = tokio::select! {
            biased;
            _ = cancel.wait_for(|cancelled| *cancelled) => return Err("upload cancelled before startup".into()),
            minted = control.json(Method::POST, Route::UploadSession, &[]) => minted?,
        };
        if minted.upload_id.is_empty()
            || minted.upload_id.len() > 8192
            || minted.upload_id.bytes().any(|byte| byte <= 32 || byte == 127)
        {
            return Err("server returned an invalid upload session ID".into());
        }
        let mut block = vec![0_u8; 64 * 1024];
        getrandom::fill(&mut block).map_err(|_| "secure randomness unavailable")?;
        let block = Bytes::from(block);
        let (state_tx, state) = watch::channel(State::default());
        let (stop_lanes, lane_stop) = watch::channel(false);
        let (stop_all, all_stop) = watch::channel(false);
        let mut owner = Self {
            transport,
            control,
            id: minted.upload_id,
            state,
            stop_lanes,
            stop_all,
            lanes: JoinSet::new(),
            progress: JoinSet::new(),
            session: None,
            retrying: Retrying::default(),
        };
        if webtransport {
            let query = [("id", owner.id.as_str())];
            let session = tokio::select! {
                biased;
                _ = cancel.wait_for(|cancelled| *cancelled) => Err("WebTransport upload cancelled during setup".into()),
                session = owner.transport.webtransport_slot(Route::WtUpload, &query) => session,
            };
            match session {
                Ok(session) => owner.session = Some(Arc::new(session)),
                Err(error) => {
                    let _ = owner.finish(false).await;
                    return Err(error);
                }
            }
        }
        {
            let transport = owner.control.clone();
            let id = owner.id.clone();
            let state = state_tx.clone();
            let mut stop = all_stop.clone();
            let session = owner.session.clone();
            owner.progress.spawn(async move {
                tokio::select! {
                    biased;
                    _ = stop.wait_for(|stopped| *stopped) => {},
                    result = progress_feed(&transport, &id, &state, session) => {
                        if let Err(error) = result {
                            fail(&state, error);
                        }
                    },
                }
            });
        }
        let mut started = Vec::with_capacity(lanes);
        for index in 0..lanes {
            let active = Arc::new(AtomicBool::new(false));
            started.push(active.clone());
            let transport = owner.transport.clone();
            let id = owner.id.clone();
            let state = state_tx.clone();
            let mut health = owner.state.clone();
            let block = block.clone();
            let mut stop = lane_stop.clone();
            let mut all_stop = all_stop.clone();
            let mut cancelled_stage = cancel.clone();
            let session = owner.session.clone();
            let retry = TransferRetry::new(owner.retrying.clone(), index);
            owner.lanes.spawn(async move {
                tokio::select! {
                    biased;
                    _ = stop.wait_for(|stopped| *stopped) => {},
                    _ = all_stop.wait_for(|stopped| *stopped) => {},
                    _ = cancelled_stage.wait_for(|cancelled| *cancelled) => {},
                    _ = health.wait_for(|state| state.error.is_some() || state.complete) => {},
                    result = async {
                        if index > 0 && !stagger.is_zero() {
                            tokio::time::sleep(stagger * index as u32).await;
                        }
                        if let Some(session) = session {
                            send_wt_reconnecting(&session, block, active, retry).await
                        } else {
                            send_lane(&transport, &id, index, block, active, retry).await
                        }
                    } => {
                        if let Err(error) = result {
                            fail(&state, error);
                        }
                    },
                }
            });
        }
        let ready = tokio::time::timeout(CONTROL_TIMEOUT, async {
            loop {
                owner.health()?;
                let state = owner.state.borrow().clone();
                let receiver_observed = state.latest.is_some_and(|count| count.bytes > 0 && count.nanos > 0);
                let all_lanes_started = started.iter().all(|active| active.load(Ordering::Acquire));
                if state.ready && receiver_observed && all_lanes_started {
                    return Ok::<(), Error>(());
                }
                tokio::select! {
                    biased;
                    _ = cancel.wait_for(|cancelled| *cancelled) => return Err("upload cancelled during startup".into()),
                    changed = owner.state.changed() => changed.map_err(|_| "upload workers ended before receiver became ready")?,
                }
            }
        }).await;
        match ready {
            Ok(Ok(())) => Ok(owner),
            result => {
                let error: Error = match result {
                    Ok(Err(error)) => error,
                    Err(error) => error.into(),
                    _ => unreachable!(),
                };
                let _ = owner.finish(false).await;
                Err(error)
            }
        }
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
            return Err(SharedFailure(error.clone()).into());
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
                self.control
                    .json::<Count>(Method::POST, Route::UploadCheckpoint, &[("id", &self.id)]),
            )
            .await;
            let count = match response {
                Ok(Ok(count)) => count,
                Ok(Err(error)) if crate::net::authentication_required(error.as_ref()).is_none() => {
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
                Some(error) => Err::<_, Error>(SharedFailure(error.clone()).into()),
                None => Ok(()),
            }
        })
        .await;
        let _ = self.stop_all.send(true);
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
impl Drop for Upload {
    fn drop(&mut self) {
        let _ = self.stop_all.send(true);
        let _ = self.stop_lanes.send(true);
        self.lanes.abort_all();
        self.progress.abort_all();
    }
}

async fn send_lane(
    transport: &Transport,
    id: &str,
    index: usize,
    block: Bytes,
    active: Arc<AtomicBool>,
    mut retry: TransferRetry,
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
        let result = transport
            .send(
                Route::Upload,
                &[("id", id), ("lane", &lane)],
                body,
                MAX_TRANSFER_BYTES,
                REQUEST_LIFETIME,
            )
            .await;
        retry.ended(result, started, moved.load(Ordering::Relaxed)).await?;
    }
}
/// Go's followUploadFeed and attach (upload.go:309-340, 365-378): a feed that fails is reopened
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

/// The receiver's progress over HTTP, a record a line of at most 64 KiB.
struct Feed {
    body: crate::transport::Body,
    line: Vec<u8>,
    chunk: Bytes,
}

impl Feed {
    /// Go's openUploadFeed (upload.go:380-400): open once the receiver answers `ready`.
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
        let mut feed = Self {
            body,
            line: Vec::new(),
            chunk: Bytes::new(),
        };
        loop {
            let event = feed.next().await?;
            let ready = matches!(event, UploadProgress::Ready);
            if apply_event(event, state)? || ready {
                return Ok(feed);
            }
        }
    }

    /// Go's read (upload.go:342-356): records until `complete`.
    async fn follow(&mut self, state: &watch::Sender<State>) -> Result<(), Error> {
        while !state.borrow().complete && !apply_event(self.next().await?, state)? {}
        Ok(())
    }

    /// The next record that decodes; the receiver sends at least one a second.
    async fn next(&mut self) -> Result<UploadProgress, Error> {
        loop {
            if self.chunk.is_empty() {
                self.chunk = tokio::time::timeout(CONTROL_TIMEOUT, self.body.chunk())
                    .await??
                    .ok_or("upload progress ended without complete")?;
            }
            let end = self.chunk.iter().position(|&byte| byte == b'\n');
            let count = end.map_or(self.chunk.len(), |end| end + 1);
            if count > MAX_LINE - self.line.len() {
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
    mut retry: TransferRetry,
) -> Result<(), Error> {
    loop {
        let session = slot.current().await;
        let started = Instant::now();
        let mut moved = false;
        let Err(error) = send_wt_lane(&session, block.clone(), &active, &mut moved).await;
        retry.ended(Err(error), started, moved).await?;
        if session.is_closed() {
            let started = Instant::now();
            if let Err(error) = slot.reconnect(&session).await {
                let retryable = !(error.is::<ConnectRejected>() || error.is::<crate::net::AuthRequired>());
                retry.retry(error, started, false, retryable).await?;
            }
        }
    }
}

async fn progress_feed(
    transport: &Transport,
    id: &str,
    state: &watch::Sender<State>,
    session: Option<Arc<SessionSlot>>,
) -> Result<(), Error> {
    if let Some(session) = session {
        let read = async {
            let mut stream = session.current().await.upload_progress().await?;
            loop {
                let event = tokio::time::timeout(CONTROL_TIMEOUT, stream.next()).await??;
                if apply_event(event, state)? {
                    return Ok::<_, Error>(());
                }
            }
        };
        match read.await {
            Ok(()) => return Ok(()),
            Err(error) if error.is::<crate::net::AuthRequired>() => return Err(error),
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
            let refusal = graphite_meter_core::failure::UploadRefusal::from_name(&code);
            return Err(Box::new(crate::failure::HttpFailure {
                status: refusal.map_or(400, |refusal| refusal.status()),
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
    use crate::transport::TRANSFER_RETRY_BACKOFF;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn http_lane_retries_dropped_streaming_request() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let transport = Transport::connect(
            crate::net::Http::new(false)?,
            &origin,
            graphite_meter_core::discovery::Protocol::Http1,
            false,
        )
        .await?;
        let active = Arc::new(AtomicBool::new(false));
        let lane = tokio::spawn(async move {
            send_lane(
                &transport,
                "upload-session",
                0,
                Bytes::from(vec![42; 64 * 1024]),
                active,
                TransferRetry::new(Retrying::default(), 0),
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
                assert!(headers.starts_with(b"POST /upload?"));
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
        let transport = Arc::new(
            Transport::connect(
                crate::net::Http::new(false)?,
                &origin,
                graphite_meter_core::discovery::Protocol::Http1,
                false,
            )
            .await?,
        );
        let (state_sender, state) = watch::channel(State::default());
        let (stop_lanes, _) = watch::channel(false);
        let (stop_all, _) = watch::channel(false);
        let upload = Upload {
            transport: transport.clone(),
            control: transport,
            id: "test-session".into(),
            state,
            stop_lanes,
            stop_all,
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
        use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
        use hyper::{body::Frame, service::service_fn};
        use hyper_util::rt::{TokioExecutor, TokioIo};
        type Payload = BoxBody<Bytes, Infallible>;
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let armed = Arc::new(AtomicBool::new(false));
        let arm = armed.clone();
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let lanes = Arc::new(AtomicBool::new(false));
                let io = Gate {
                    inner: socket,
                    lanes: lanes.clone(),
                    armed: arm.clone(),
                };
                let service = service_fn(move |request: http::Request<hyper::body::Incoming>| {
                    let lanes = lanes.clone();
                    async move {
                        let json =
                            |body: &'static str| -> Payload { Full::new(Bytes::from_static(body.as_bytes())).boxed() };
                        let body = match request.uri().path() {
                            "/upload/session" => json(r#"{"uploadId":"fixture"}"#),
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
                        Ok::<_, Infallible>(http::Response::new(body))
                    }
                });
                tokio::spawn(
                    hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(io), service),
                );
            }
        });
        let transport = Arc::new(
            Transport::connect(
                crate::net::Http::new(false)?,
                &origin,
                graphite_meter_core::discovery::Protocol::Http2,
                false,
            )
            .await?,
        );
        let (_stop, cancel) = watch::channel(false);
        let upload = tokio::time::timeout(
            Duration::from_secs(5),
            Upload::start(transport, 2, Duration::ZERO, cancel),
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
        let transport = Transport::connect(
            crate::net::Http::new(false)?,
            &origin,
            graphite_meter_core::discovery::Protocol::Http1,
            false,
        )
        .await?;
        let (state, observed) = watch::channel(State::default());
        progress_loop(&transport, "test-session", &state).await?;
        server.await??;
        let observed = observed.borrow();
        assert!(observed.complete);
        let latest = observed.latest.unwrap();
        assert_eq!((latest.bytes, latest.nanos), (12, 30));
        Ok(())
    }

    /// A feed that cannot open is tried again for 2 s at Go's pace (upload.go:365-378,
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
        let transport = Transport::connect(
            crate::net::Http::new(false)?,
            &origin,
            graphite_meter_core::discovery::Protocol::Http1,
            false,
        )
        .await?;
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
