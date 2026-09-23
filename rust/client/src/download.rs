//! Owned download lanes. Only received bytes contribute to measurement.
use crate::{Error, net::Http, transport::Transport, webtransport::Session};
use graphite_meter_core::{
    discovery::{ThroughputTarget, ThroughputTransport},
    origin::canonical_origin,
    route::Route,
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
    time::timeout,
};

pub struct Download {
    bytes: Arc<AtomicU64>,
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
            tasks: JoinSet::new(),
        };
        let (ready, mut received) = mpsc::channel(lanes);
        for lane in 0..lanes {
            let transport = transport.clone();
            let bytes = owner.bytes.clone();
            let ready = ready.clone();
            let mut cancel = cancel.clone();
            owner.tasks.spawn(async move {
                let transfer = async {
                    if lane > 0 && !stagger.is_zero() {
                        tokio::time::sleep(stagger * lane as u32).await;
                    }
                    let lane = lane.to_string();
                    let mut announced = false;
                    loop {
                        const LIMIT: u64 = 64 * 1024 * 1024 * 1024;
                        let mut body = transport
                            .receive(
                                Method::GET,
                                Route::Download,
                                &[("bytes", "68719476736"), ("lane", &lane)],
                                LIMIT,
                                duration,
                            )
                            .await?;
                        let mut received = 0_u64;
                        while let Some(chunk) = body.chunk().await? {
                            bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                            received += chunk.len() as u64;
                            if !announced && !chunk.is_empty() {
                                ready
                                    .send(())
                                    .await
                                    .map_err(|_| "download readiness receiver closed")?;
                                announced = true;
                            }
                        }
                        if received != LIMIT {
                            return Err("download ended before its declared byte count".into());
                        }
                    }
                };
                tokio::select! {
                    biased;
                    _ = cancel.wait_for(|value| *value) => Ok(()),
                    result = transfer => result,
                }
            });
        }
        drop(ready);
        timeout(Duration::from_secs(10), async {
            for _ in 0..lanes {
                tokio::select! {
                    value = received.recv() => value.ok_or("download ended before readiness")?,
                    task = owner.tasks.join_next() => {
                        task.ok_or("no download lanes")???;
                        return Err::<(), Error>("download cancelled before readiness".into());
                    }
                }
            }
            Ok(())
        })
        .await??;
        Ok(owner)
    }

    /// Start bounded WebTransport lanes using the same received-byte owner as HTTP.
    /// Each connection has one session and at most sixteen readers. A failed lane
    /// remains an error; data received before failure stays in the counter.
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
        let datagrams = match target.transport {
            ThroughputTransport::WebTransport => false,
            ThroughputTransport::WebTransportDatagram => true,
            ThroughputTransport::FetchStream => {
                return Err("WebTransport download requires a WebTransport target".into());
            }
        };
        let origin = canonical_origin(&target.base_url)?;
        let mut owner = Self {
            bytes: Arc::new(AtomicU64::new(0)),
            tasks: JoinSet::new(),
        };
        let (ready, mut received) = mpsc::channel(lanes);
        let lane_cancel = cancel.clone();
        let start = async {
            for first in (0..lanes).step_by(WT_LANES_PER_SESSION) {
                let group = (lanes - first).min(WT_LANES_PER_SESSION);
                let target = format!(
                    "{origin}/wt/download?bytes={WT_STREAM_BYTES}&streams={group}&datagrams={}",
                    u8::from(datagrams)
                );
                let session = Arc::new(
                    Session::dial(http, &target, insecure, Duration::from_secs(10)).await?,
                );
                for _ in 0..group {
                    let session = session.clone();
                    let bytes = owner.bytes.clone();
                    let ready = ready.clone();
                    let mut cancel = lane_cancel.clone();
                    owner.tasks.spawn(async move {
                        tokio::select! {biased;
                            _ = cancel.wait_for(|value| *value) => Ok(()),
                            result = timeout(duration, receive_webtransport(session, bytes, ready, datagrams)) => result?,
                        }
                    });
                }
            }
            drop(ready);
            for _ in 0..lanes {
                tokio::select! {
                    value = received.recv() => value.ok_or("WebTransport download ended before readiness")?,
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

    pub fn health(&mut self) -> Result<(), Error> {
        if let Some(task) = self.tasks.try_join_next() {
            task??;
            return Err("download lane ended before stage boundary".into());
        }
        Ok(())
    }

    pub async fn stop(mut self) {
        self.tasks.shutdown().await;
    }
}

const WT_LANES_PER_SESSION: usize = 16;
const WT_STREAM_BYTES: u64 = 64 * 1024 * 1024;

async fn receive_webtransport(
    session: Arc<Session>,
    bytes: Arc<AtomicU64>,
    ready: mpsc::Sender<()>,
    datagrams: bool,
) -> Result<(), Error> {
    let mut announced = false;
    loop {
        if datagrams {
            let chunk = session.recv_datagram().await?;
            record_webtransport(&bytes, &ready, &mut announced, chunk.len())?;
            continue;
        }
        let mut stream = session.accept_uni().await?;
        let mut received = 0_u64;
        while let Some(chunk) = stream.read_chunk().await? {
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or("download byte count overflow")?;
            record_webtransport(&bytes, &ready, &mut announced, chunk.len())?;
            if received > WT_STREAM_BYTES {
                return Err("WebTransport download exceeded its declared byte count".into());
            }
        }
        if received != WT_STREAM_BYTES {
            return Err("WebTransport download ended before its declared byte count".into());
        }
    }
}

fn record_webtransport(
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    announced: &mut bool,
    count: usize,
) -> Result<(), Error> {
    bytes.fetch_add(count as u64, Ordering::Relaxed);
    if !*announced && count > 0 {
        ready
            .try_send(())
            .map_err(|_| "download readiness receiver closed")?;
        *announced = true;
    }
    Ok(())
}
