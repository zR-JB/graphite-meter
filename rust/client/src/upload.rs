//! Stage-owned upload lanes and authoritative receiver evidence.
use crate::{
    Error,
    failure::SharedFailure,
    transport::{Retrying, TransferRetry, Transport},
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
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LINE: usize = 64 * 1024;
const CHECKPOINT_RETRY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug)]
pub struct ReceiverProgress {
    pub bytes: u64,
    pub nanos: u64,
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
        let control = if !webtransport && transport.is_http3() {
            transport.isolated_connection().await?
        } else {
            transport.clone()
        };
        let minted: Minted = tokio::select! {
            biased;
            () = cancelled(&mut cancel) => return Err("upload cancelled before startup".into()),
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
                () = cancelled(&mut cancel) => Err("WebTransport upload cancelled during setup".into()),
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
                    () = cancelled(&mut stop) => {},
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
                    () = cancelled(&mut stop) => {},
                    () = cancelled(&mut all_stop) => {},
                    () = cancelled(&mut cancelled_stage) => {},
                    () = failed(&mut health) => {},
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
                    () = cancelled(&mut cancel) => return Err("upload cancelled during startup".into()),
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
    pub fn latest(&self) -> Option<ReceiverProgress> {
        self.state.borrow().latest
    }
    pub fn observed(&self) -> Option<ObservedUpload> {
        self.latest().map(|value| ObservedUpload {
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
    pub async fn finish(mut self, confirm: bool) -> Result<Option<ReceiverProgress>, Error> {
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
            loop {
                let state = self.state.borrow().clone();
                if let Some(error) = state.error {
                    return Err::<_, Error>(SharedFailure(error).into());
                }
                if state.complete || !confirm {
                    return Ok(state.latest);
                }
                self.state
                    .changed()
                    .await
                    .map_err(|_| "upload progress ended without complete")?;
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
        if let Err(error) = result {
            let retryable = transport.retryable_transfer_error(&error);
            retry
                .retry(error, started, moved.load(Ordering::Relaxed), retryable)
                .await?;
        } else {
            retry.progressed();
        }
    }
}
async fn progress_loop(transport: &Transport, id: &str, state: &watch::Sender<State>) -> Result<(), Error> {
    let mut recovery = None;
    loop {
        let result = read_progress(transport, id, state, &mut recovery).await;
        match result {
            Ok(()) => return Ok(()),
            Err(error) if error.is::<crate::net::AuthRequired>() => return Err(error),
            Err(error) => {
                let deadline = *recovery.get_or_insert_with(|| Instant::now() + Duration::from_secs(2));
                if Instant::now() >= deadline {
                    return Err(crate::failure::NotReplaced("upload progress", Some(error)).into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
async fn read_progress(
    transport: &Transport,
    id: &str,
    state: &watch::Sender<State>,
    recovery: &mut Option<Instant>,
) -> Result<(), Error> {
    let ready_deadline = recovery.unwrap_or_else(|| Instant::now() + CONTROL_TIMEOUT);
    let mut body = tokio::time::timeout_at(
        ready_deadline,
        transport.receive(
            Method::GET,
            Route::UploadProgress,
            &[("id", id)],
            u64::MAX,
            Duration::from_secs(24 * 60 * 60),
        ),
    )
    .await??;
    let mut line = Vec::new();
    let mut ready = false;
    loop {
        let deadline = if ready {
            Instant::now() + CONTROL_TIMEOUT
        } else {
            ready_deadline
        };
        let Some(chunk) = tokio::time::timeout_at(deadline, body.chunk()).await?? else {
            break;
        };
        for part in chunk.split_inclusive(|byte| *byte == b'\n') {
            if part.len() > MAX_LINE - line.len() {
                return Err("upload progress line exceeds 64 KiB".into());
            }
            line.extend_from_slice(part);
            if !line.ends_with(b"\n") {
                continue;
            }
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                continue;
            }
            let event = wire::decode_upload_progress(&line);
            line.clear();
            let Ok(event) = event else {
                continue;
            };
            if apply_event(event, state, &mut ready)? {
                return Ok(());
            }
            if ready {
                *recovery = None;
            }
        }
    }
    Err("upload progress ended without complete".into())
}
fn fail(state: &watch::Sender<State>, error: Error) {
    state.send_modify(|state| {
        if state.error.is_none() {
            state.error = Some(Arc::new(error));
        }
    });
}
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    while !*cancel.borrow_and_update() {
        if cancel.changed().await.is_err() {
            break;
        }
    }
}
async fn failed(state: &mut watch::Receiver<State>) {
    loop {
        if state.borrow().error.is_some() || state.borrow().complete {
            return;
        }
        if state.changed().await.is_err() {
            return;
        }
    }
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
        let retryable = session.retryable_failure(&error);
        retry.retry(error, started, moved, retryable).await?;
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
            let mut ready = false;
            loop {
                let event = tokio::time::timeout(CONTROL_TIMEOUT, stream.next()).await??;
                if apply_event(event, state, &mut ready)? {
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
fn apply_event(event: UploadProgress, state: &watch::Sender<State>, ready: &mut bool) -> Result<bool, Error> {
    match event {
        UploadProgress::Ready => {
            *ready = true;
            state.send_modify(|state| state.ready = true);
        }
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
}
