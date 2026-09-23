//! Stage-owned upload lanes and authoritative receiver evidence.
use crate::{Error, transport::Transport};
use bytes::Bytes;
use graphite_meter_core::{
    measurement::{ObservedUpload, ReceiverSnapshot},
    route::Route,
    wire::{self, MAX_UPLOAD_COUNTER, UploadProgress},
};
use http::Method;
use serde::Deserialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::watch, task::JoinSet, time::Instant};

const REQUEST_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const REQUEST_LIFETIME: Duration = Duration::from_secs(120);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LINE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ReceiverProgress {
    pub bytes: u64,
    pub nanos: u64,
    pub received_at_nanos: u64,
}
#[derive(Clone, Default)]
struct State {
    ready: bool,
    complete: bool,
    latest: Option<ReceiverProgress>,
    error: Option<Arc<Error>>,
}

struct SharedFailure(Arc<Error>);
impl std::fmt::Debug for SharedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upload task failed")
    }
}
impl std::fmt::Display for SharedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.0.as_ref(), f)
    }
}
impl std::error::Error for SharedFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

/// Drop cancels every owned task. Call finish() to drain tasks and explicitly
/// finish the remote aggregate; cancelled finish/drop relies on server expiry.
pub struct Upload {
    transport: Arc<Transport>,
    id: String,
    epoch: Instant,
    state: watch::Receiver<State>,
    stop_lanes: watch::Sender<bool>,
    stop_all: watch::Sender<bool>,
    lanes: JoinSet<()>,
    progress: JoinSet<()>,
    session: Option<Arc<crate::webtransport::Session>>,
}

impl Upload {
    pub async fn start(
        transport: Arc<Transport>,
        lanes: usize,
        epoch: Instant,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        Self::start_staggered(transport, lanes, epoch, Duration::ZERO, cancel).await
    }

    /// Stagger first HTTP requests inside the stage-owned cancellation scope.
    pub async fn start_staggered(
        transport: Arc<Transport>,
        lanes: usize,
        epoch: Instant,
        stagger: Duration,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        Self::start_inner(transport, lanes, epoch, cancel, None, stagger).await
    }
    pub async fn start_webtransport(
        transport: Arc<Transport>,
        lanes: usize,
        epoch: Instant,
        datagrams: bool,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        // Datagrams share one receiver byte counter and have no stream identifiers.
        // The sixteen-stream limit therefore applies only to reliable upload lanes.
        if !datagrams && lanes > 16 {
            return Err("WebTransport upload supports at most sixteen streams per session".into());
        }
        Self::start_inner(
            transport,
            lanes,
            epoch,
            cancel,
            Some(datagrams),
            Duration::ZERO,
        )
        .await
    }
    async fn start_inner(
        transport: Arc<Transport>,
        lanes: usize,
        epoch: Instant,
        mut cancel: watch::Receiver<bool>,
        datagrams: Option<bool>,
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
        let minted: Minted = tokio::select! {
            biased;
            () = cancelled(&mut cancel) => return Err("upload cancelled before startup".into()),
            minted = transport.json(Method::POST, Route::UploadSession, &[]) => minted?,
        };
        if minted.upload_id.is_empty()
            || minted.upload_id.len() > 8192
            || minted
                .upload_id
                .bytes()
                .any(|byte| byte <= 32 || byte == 127)
        {
            return Err("server returned an invalid upload session ID".into());
        }
        let mut block = vec![0_u8; 64 * 1024];
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut block)
            .map_err(|_| "secure randomness unavailable")?;
        let block = Bytes::from(block);
        let (state_tx, state) = watch::channel(State::default());
        let (stop_lanes, lane_stop) = watch::channel(false);
        let (stop_all, all_stop) = watch::channel(false);
        let mut owner = Self {
            transport,
            id: minted.upload_id,
            epoch,
            state,
            stop_lanes,
            stop_all,
            lanes: JoinSet::new(),
            progress: JoinSet::new(),
            session: None,
        };
        if let Some(datagrams) = datagrams {
            let flag = if datagrams { "1" } else { "0" };
            let query = [("id", owner.id.as_str()), ("datagrams", flag)];
            let session = tokio::select! {
                biased;
                () = cancelled(&mut cancel) => return Err("WebTransport upload cancelled during setup".into()),
                session = owner.transport.webtransport(Route::WtUpload, &query) => session?,
            };
            owner.session = Some(Arc::new(session));
        }
        {
            let transport = owner.transport.clone();
            let id = owner.id.clone();
            let state = state_tx.clone();
            let mut stop = all_stop.clone();
            let session = owner.session.clone();
            owner.progress.spawn(async move {
                tokio::select! {
                    biased;
                    () = cancelled(&mut stop) => {},
                    result = progress_feed(&transport, &id, epoch, &state, session) => {
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
                            send_wt_lane(&session, datagrams.unwrap_or(false), block, active).await
                        } else {
                            send_lane(&transport, &id, index, block, active).await
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
                let _ = owner.finish().await;
                Err(error)
            }
        }
    }
    pub fn id(&self) -> &str {
        &self.id
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
    pub async fn checkpoint(&self) -> Result<ReceiverSnapshot, Error> {
        self.health()?;
        #[derive(Deserialize)]
        struct Count {
            bytes: u64,
            nanos: u64,
        }
        let requested_at_nanos = elapsed(self.epoch)?;
        let count: Count = self
            .transport
            .json(Method::POST, Route::UploadCheckpoint, &[("id", &self.id)])
            .await?;
        let received_at_nanos = elapsed(self.epoch)?;
        if count.bytes > MAX_UPLOAD_COUNTER || count.nanos == 0 || count.nanos > MAX_UPLOAD_COUNTER
        {
            return Err("invalid receiver checkpoint counters".into());
        }
        self.health()?;
        Ok(ReceiverSnapshot {
            id: self.id.clone(),
            bytes: count.bytes,
            nanos: count.nanos,
            requested_at_nanos,
            received_at_nanos,
        })
    }
    pub async fn finish(mut self) -> Result<Option<ReceiverProgress>, Error> {
        let result = tokio::time::timeout(CONTROL_TIMEOUT, async {
            let _ = self.stop_lanes.send(true);
            while let Some(result) = self.lanes.join_next().await {
                result?;
            }
            let mut response = self
                .transport
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
                if state.complete {
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
) -> Result<(), Error> {
    let lane = index.to_string();
    loop {
        let active = active.clone();
        let block = block.clone();
        let body = futures_util::stream::unfold(
            (block, REQUEST_BYTES, active),
            |(block, remaining, active)| async move {
                if remaining == 0 {
                    return None;
                }
                active.store(true, Ordering::Release);
                let size = remaining.min(block.len() as u64) as usize;
                Some((
                    Ok::<_, Error>(block.slice(..size)),
                    (block, remaining - size as u64, active),
                ))
            },
        );
        transport
            .send(
                Route::Upload,
                &[("id", id), ("lane", &lane)],
                body,
                REQUEST_BYTES,
                REQUEST_LIFETIME,
            )
            .await?;
    }
}
async fn progress_loop(
    transport: &Transport,
    id: &str,
    epoch: Instant,
    state: &watch::Sender<State>,
) -> Result<(), Error> {
    let mut recovery = None;
    loop {
        let result = read_progress(transport, id, epoch, state, &mut recovery).await;
        match result {
            Ok(()) => return Ok(()),
            Err(error)
                if error.is::<crate::net::AuthRequired>() || error.is::<InvalidProgress>() =>
            {
                return Err(error);
            }
            Err(_) => {
                let deadline =
                    *recovery.get_or_insert_with(|| Instant::now() + Duration::from_secs(2));
                if Instant::now() >= deadline {
                    return Err("upload progress did not recover within two seconds".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
#[derive(Debug)]
struct InvalidProgress(&'static str);
impl std::fmt::Display for InvalidProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidProgress {}

async fn read_progress(
    transport: &Transport,
    id: &str,
    epoch: Instant,
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
                return Err(InvalidProgress("upload progress line exceeds 64 KiB").into());
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
            let event = wire::decode_upload_progress(&line)
                .map_err(|_| InvalidProgress("invalid upload progress record"))?;
            line.clear();
            if apply_event(event, epoch, state, &mut ready)? {
                return Ok(());
            }
            if ready {
                *recovery = None;
            }
        }
    }
    Err("upload progress ended without complete".into())
}
fn elapsed(epoch: Instant) -> Result<u64, Error> {
    Ok(
        u64::try_from(Instant::now().saturating_duration_since(epoch).as_nanos())
            .map_err(|_| "client measurement clock overflow")?,
    )
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
    datagrams: bool,
    block: Bytes,
    active: Arc<AtomicBool>,
) -> Result<(), Error> {
    if datagrams {
        loop {
            let size = session
                .max_datagram_size()
                .filter(|size| *size > 0)
                .ok_or("WebTransport peer has no datagram capacity")?
                .min(block.len());
            session.send_datagram(&block[..size]).await?;
            active.store(true, Ordering::Release);
            tokio::task::yield_now().await;
        }
    } else {
        loop {
            let mut stream = session.open_uni().await?;
            let mut remaining = REQUEST_BYTES;
            while remaining > 0 {
                let size = remaining.min(block.len() as u64) as usize;
                stream.write_chunk(block.slice(..size)).await?;
                remaining -= size as u64;
                active.store(true, Ordering::Release);
            }
            stream.finish()?;
        }
    }
}
async fn progress_feed(
    transport: &Transport,
    id: &str,
    epoch: Instant,
    state: &watch::Sender<State>,
    session: Option<Arc<crate::webtransport::Session>>,
) -> Result<(), Error> {
    if let Some(session) = session {
        let read = async {
            let mut stream = session.upload_progress().await?;
            let mut ready = false;
            loop {
                let event = tokio::time::timeout(CONTROL_TIMEOUT, stream.next()).await??;
                if apply_event(event, epoch, state, &mut ready)? {
                    return Ok::<_, Error>(());
                }
            }
        };
        match read.await {
            Ok(()) => return Ok(()),
            Err(error)
                if error.is::<InvalidProgress>() || error.is::<crate::net::AuthRequired>() =>
            {
                return Err(error);
            }
            Err(_) => {}
        }
        // Reattach only the receiver's control feed. Payload lanes remain WT.
    }
    progress_loop(transport, id, epoch, state).await
}
fn apply_event(
    event: UploadProgress,
    epoch: Instant,
    state: &watch::Sender<State>,
    ready: &mut bool,
) -> Result<bool, Error> {
    match event {
        UploadProgress::Ready => {
            *ready = true;
            state.send_modify(|state| state.ready = true);
        }
        UploadProgress::Error { .. } => {
            return Err(InvalidProgress("upload receiver refused the operation").into());
        }
        UploadProgress::Progress { bytes, nanos } | UploadProgress::Complete { bytes, nanos } => {
            if !*ready {
                return Err(InvalidProgress("upload progress preceded ready").into());
            }
            let old = state.borrow().latest;
            if old.is_some_and(|old| bytes < old.bytes || nanos < old.nanos) {
                return Err(InvalidProgress("upload receiver counters regressed").into());
            }
            let count = ReceiverProgress {
                bytes,
                nanos,
                received_at_nanos: elapsed(epoch)?,
            };
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

    #[test]
    fn receiver_evidence_requires_ready_and_rejects_regression() {
        let epoch = Instant::now();
        let (state, observed) = watch::channel(State::default());
        let mut ready = false;
        assert!(
            apply_event(
                UploadProgress::Progress {
                    bytes: 10,
                    nanos: 20
                },
                epoch,
                &state,
                &mut ready
            )
            .is_err()
        );
        assert!(observed.borrow().latest.is_none());
        apply_event(UploadProgress::Ready, epoch, &state, &mut ready).unwrap();
        apply_event(
            UploadProgress::Progress {
                bytes: 10,
                nanos: 20,
            },
            epoch,
            &state,
            &mut ready,
        )
        .unwrap();
        for event in [
            UploadProgress::Progress {
                bytes: 9,
                nanos: 21,
            },
            UploadProgress::Complete {
                bytes: 11,
                nanos: 19,
            },
        ] {
            assert!(apply_event(event, epoch, &state, &mut ready).is_err());
        }
        assert_eq!(observed.borrow().latest.unwrap().bytes, 10);
        assert!(!observed.borrow().complete);
        assert!(
            apply_event(
                UploadProgress::Complete {
                    bytes: 10,
                    nanos: 20
                },
                epoch,
                &state,
                &mut ready
            )
            .unwrap()
        );
        assert!(observed.borrow().complete);
    }
}
