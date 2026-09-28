//! Owned download lanes. Only received bytes contribute to measurement.
use crate::{
    Error,
    failure::MeasurementFailure,
    net::Http,
    transport::{Retrying, TransferRetry, Transport},
    webtransport::{ConnectRejected, Session, SessionSlot},
};
use graphite_meter_core::{
    discovery::{ThroughputTarget, ThroughputTransport},
    failure::FailureReason,
    origin::canonical_origin,
    route::Route,
    wire::{MAX_TRANSFER_BYTES, MAX_WEBTRANSPORT_STREAMS},
};
use http::Method;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{Instant, timeout},
};

pub struct Download {
    bytes: Arc<AtomicU64>,
    retrying: Retrying,
    tasks: JoinSet<Result<(), Error>>,
}

impl Download {
    pub async fn start(
        transport: Arc<Transport>,
        lanes: usize,
        duration: Duration,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        Self::start_staggered(transport, lanes, duration, Duration::ZERO, cancel).await
    }

    /// Delay each successive lane before its first request; cancellation covers the delay.
    pub async fn start_staggered(
        transport: Arc<Transport>,
        lanes: usize,
        duration: Duration,
        stagger: Duration,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) || stagger > Duration::from_millis(75) {
            return Err("invalid download lane count or stagger".into());
        }
        let bytes = Arc::new(AtomicU64::new(0));
        let mut owner = Self {
            bytes,
            retrying: Retrying::default(),
            tasks: JoinSet::new(),
        };
        let (ready, mut received) = mpsc::channel(lanes);
        for lane in 0..lanes {
            let transport = transport.clone();
            let bytes = owner.bytes.clone();
            let ready = ready.clone();
            let mut cancel = cancel.clone();
            let retry = TransferRetry::new(owner.retrying.clone(), lane);
            owner.tasks.spawn(async move {
                let transfer = async {
                    if lane > 0 && !stagger.is_zero() {
                        tokio::time::sleep(stagger * lane as u32).await;
                    }
                    receive_http_lane(&transport, lane, &bytes, &ready, duration, retry).await
                };
                tokio::select! {
                    biased;
                    _ = cancel.wait_for(|value| *value) => Ok(()),
                    result = transfer => result,
                }
            });
        }
        drop(ready);
        let mut ready_lanes = 0;
        let readiness = async {
            for _ in 0..lanes {
                tokio::select! {
                    // Lanes drop their sender before their task completes; report the lane's cause.
                    value = received.recv() => if value.is_none() {
                        owner.tasks.join_next().await.ok_or("no download lanes")???;
                        return Err("download ended before readiness".into());
                    },
                    task = owner.tasks.join_next() => {
                        task.ok_or("no download lanes")???;
                        return Err::<(), Error>("download cancelled before readiness".into());
                    }
                }
                ready_lanes += 1;
            }
            Ok(())
        };
        match timeout(Duration::from_secs(10), readiness).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(format!(
                    "download readiness timed out: {ready_lanes}/{lanes} lanes received response headers"
                )
                .into());
            }
        }
        Ok(owner)
    }

    /// Start bounded WebTransport lanes using the same received-byte owner as HTTP.
    /// Each connection has one session and at most sixteen readers. Lost
    /// connections are replaced once per group; observed bytes remain counted.
    pub async fn start_webtransport(
        http: &Http,
        target: &ThroughputTarget,
        lanes: usize,
        duration: Duration,
        insecure: bool,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) || duration.is_zero() {
            return Err("invalid WebTransport download lanes or duration".into());
        }
        if target.transport != ThroughputTransport::WebTransport {
            return Err("WebTransport download requires a stream target".into());
        }
        let origin = canonical_origin(&target.base_url)?;
        let mut owner = Self {
            bytes: Arc::new(AtomicU64::new(0)),
            retrying: Retrying::default(),
            tasks: JoinSet::new(),
        };
        let (ready, mut received) = mpsc::channel(lanes);
        let lane_cancel = cancel.clone();
        let start = async {
            for first in (0..lanes).step_by(MAX_WEBTRANSPORT_STREAMS) {
                let group = (lanes - first).min(MAX_WEBTRANSPORT_STREAMS);
                let target = format!("{origin}/wt/download?bytes={WT_STREAM_BYTES}&streams={group}");
                let slot = Arc::new(SessionSlot::dial(http, target, insecure).await?);
                for lane in first..first + group {
                    let slot = slot.clone();
                    let bytes = owner.bytes.clone();
                    let ready = ready.clone();
                    let mut cancel = lane_cancel.clone();
                    let retry = TransferRetry::new(owner.retrying.clone(), lane);
                    owner.tasks.spawn(async move {
                        tokio::select! {biased;
                            _ = cancel.wait_for(|value| *value) => Ok(()),
                            result = timeout(duration, receive_webtransport(slot, bytes, ready, retry)) => result?,
                        }
                    });
                }
            }
            drop(ready);
            for _ in 0..lanes {
                tokio::select! {
                    value = received.recv() => if value.is_none() {
                        owner.tasks.join_next().await.ok_or("no WebTransport download lanes")???;
                        return Err("WebTransport download ended before readiness".into());
                    },
                    task = owner.tasks.join_next() => {
                        task.ok_or("no WebTransport download lanes")???;
                        return Err::<(), Error>("WebTransport download cancelled before readiness".into());
                    }
                }
            }
            Ok(())
        };
        tokio::select! {biased;
            _ = cancel.wait_for(|value| *value) => return Err("download cancelled before readiness".into()),
            result = timeout(Duration::from_secs(10), start) => result??,
        }
        Ok(owner)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn retrying(&self) -> Option<Error> {
        self.retrying.failure()
    }

    pub fn health(&mut self) -> Result<(), Error> {
        if let Some(task) = self.tasks.try_join_next() {
            task??;
            return Err(Box::new(MeasurementFailure(FailureReason::ConnectionLost)));
        }
        Ok(())
    }

    pub async fn stop(mut self) {
        self.tasks.shutdown().await;
    }
}

async fn receive_http_lane(
    transport: &Transport,
    lane: usize,
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    duration: Duration,
    mut retry: TransferRetry,
) -> Result<(), Error> {
    let lane = lane.to_string();
    let requested_bytes = MAX_TRANSFER_BYTES.to_string();
    let mut announced = false;
    loop {
        let started = Instant::now();
        let mut moved = false;
        let attempt = async {
            let mut body = transport
                .receive(
                    Method::GET,
                    Route::Download,
                    &[("bytes", &requested_bytes), ("lane", &lane)],
                    MAX_TRANSFER_BYTES,
                    duration,
                )
                .await?;
            if !announced {
                ready.send(()).await.map_err(|_| "download readiness receiver closed")?;
                announced = true;
            }
            let mut received = 0;
            while let Some(chunk) = body.chunk().await? {
                bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                received += chunk.len() as u64;
                moved |= !chunk.is_empty();
            }
            if received != MAX_TRANSFER_BYTES {
                return Err("download ended before its declared byte count".into());
            }
            Ok::<(), Error>(())
        };
        match attempt.await {
            Ok(()) => retry.progressed(),
            Err(error) => {
                let retryable = transport.retryable_transfer_error(&error);
                retry.retry(error, started, moved, retryable).await?;
            }
        }
    }
}

const WT_STREAM_BYTES: u64 = 64 * 1024 * 1024;

async fn receive_webtransport(
    slot: Arc<SessionSlot>,
    bytes: Arc<AtomicU64>,
    ready: mpsc::Sender<()>,
    mut retry: TransferRetry,
) -> Result<(), Error> {
    let mut announced = false;
    loop {
        let session = slot.current().await;
        let started = Instant::now();
        let mut moved = false;
        let result = receive_webtransport_chunk(&session, &bytes, &ready, &mut announced, &mut moved).await;
        let Err(error) = result else {
            retry.progressed();
            continue;
        };
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

async fn receive_webtransport_chunk(
    session: &Session,
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    announced: &mut bool,
    moved: &mut bool,
) -> Result<(), Error> {
    let mut stream = session.accept_uni().await?;
    let mut received = 0_u64;
    while let Some(chunk) = stream.read_chunk().await? {
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or("download byte count overflow")?;
        *moved |= !chunk.is_empty();
        record_webtransport(bytes, ready, announced, chunk.len())?;
        if received > WT_STREAM_BYTES {
            return Err("WebTransport download exceeded its declared byte count".into());
        }
    }
    if received != WT_STREAM_BYTES {
        return Err("WebTransport download ended before its declared byte count".into());
    }
    Ok(())
}

fn record_webtransport(
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    announced: &mut bool,
    count: usize,
) -> Result<(), Error> {
    bytes.fetch_add(count as u64, Ordering::Relaxed);
    if !*announced && count > 0 {
        ready.try_send(()).map_err(|_| "download readiness receiver closed")?;
        *announced = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TRANSFER_RETRY_BACKOFF;
    use graphite_meter_core::discovery::Protocol;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn http_lane_preserves_received_bytes_across_partial_responses() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let mut closed = tokio::time::Instant::now();
            for (attempt, lasts) in [Duration::ZERO, 2 * TRANSFER_RETRY_BACKOFF, Duration::ZERO]
                .into_iter()
                .enumerate()
            {
                let (mut stream, _) = listener.accept().await?;
                match attempt {
                    1 => assert!(closed.elapsed() >= TRANSFER_RETRY_BACKOFF),
                    2 => assert!(closed.elapsed() < TRANSFER_RETRY_BACKOFF),
                    _ => {}
                }
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).await?;
                assert!(request[..count].starts_with(b"GET /download?"));
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n")
                    .await?;
                stream.write_all(&[42; 1024]).await?;
                tokio::time::sleep(lasts).await;
                drop(stream);
                closed = tokio::time::Instant::now();
            }
            Ok::<_, Error>(())
        });
        let transport = Arc::new(Transport::connect(Http::new(false)?, &origin, Protocol::Http1, false).await?);
        let (_stop, cancelled) = watch::channel(false);
        let mut download = Download::start(transport, 1, Duration::from_secs(5), cancelled).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while download.bytes() < 3072 {
                download.health()?;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, Error>(())
        })
        .await??;
        download.stop().await;
        server.await??;
        Ok(())
    }
}
