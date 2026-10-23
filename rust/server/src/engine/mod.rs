//! Transport-neutral measurement work: download payloads, the latency reflector, upload aggregates and feeds.

pub mod download;
mod uploads;

pub use download::{Block, DownloadSource};
pub use uploads::Uploads;

use crate::lane::Lane;
use bytes::Bytes;
use graphite_meter_proto::{
    bus::Ping,
    refusal::Refusal,
    upload::{HEARTBEAT, Record},
};
use std::{
    future::Future,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, sleep_until};
use uploads::{Aggregate, Phase};

/// One direction's counts while verbose logs are on; clones share them.
#[derive(Debug, Clone, Default)]
pub struct Meter(Option<Arc<Counts>>);

#[derive(Debug, Default)]
struct Counts {
    bytes: AtomicU64,
    transfers: AtomicUsize,
}

#[derive(Debug)]
pub struct Transfer(Arc<Counts>);

impl Meter {
    pub fn new(enabled: bool) -> Self {
        Self(enabled.then(Arc::default))
    }

    /// A running transfer, one per lane, which the lane's streams share.
    #[must_use = "dropping it ends the transfer"]
    pub fn open(&self) -> Option<Arc<Transfer>> {
        let counts = self.0.as_ref()?;
        counts.transfers.fetch_add(1, Ordering::Relaxed);
        Some(Arc::new(Transfer(counts.clone())))
    }

    /// The window's line, e.g. `1.20 Gbit/s · 2 transfers · 150.00 MB in 1.0 s`; none if idle.
    pub fn line(&self, window: Duration) -> Option<String> {
        let counts = self.0.as_ref()?;
        let bytes = counts.bytes.swap(0, Ordering::Relaxed);
        let transfers = counts.transfers.load(Ordering::Relaxed);
        if bytes == 0 && transfers == 0 {
            return None;
        }
        let gbits = bytes as f64 * 8.0 / window.as_secs_f64() / 1e9;
        let megabytes = bytes as f64 / 1e6;
        let seconds = window.as_secs_f64();
        Some(format!(
            "{gbits:.2} Gbit/s · {transfers} transfers · {megabytes:.2} MB in {seconds:.1} s"
        ))
    }
}

impl Transfer {
    pub fn record(&self, bytes: usize) {
        self.0.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.0.transfers.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The PONG for a bus message received at `received` (`api/wire.md#reflector-handling-time`); `None` if malformed.
pub fn reflect(message: &[u8], received: std::time::Instant) -> Option<String> {
    let ping = Ping::decode(message)?;
    let handling = u64::try_from(received.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Some(ping.reply(handling).encode())
}

/// A joined data lane; dropping it leaves the aggregate, which may then complete.
#[must_use = "dropping it leaves the aggregate"]
pub struct UploadSink {
    aggregate: Arc<Aggregate>,
    lane: Lane,
    bytes: u64,
    transfer: Option<Arc<Transfer>>,
}

impl UploadSink {
    fn new(aggregate: Arc<Aggregate>, lane: Lane, transfer: Option<Arc<Transfer>>) -> Self {
        Self {
            aggregate,
            lane,
            bytes: 0,
            transfer,
        }
    }

    /// Counts bytes received from the peer, once per chunk or batch; receiving any is the lane's movement.
    pub fn record(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        self.lane.moved();
        self.aggregate.record(bytes as u64);
        self.bytes += bytes as u64;
        if let Some(transfer) = &self.transfer {
            transfer.record(bytes);
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Completes once the upload is finalized or expired: the end of a lane without a stream FIN.
    pub fn finished(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        let aggregate = self.aggregate.clone();
        async move {
            loop {
                let mut changed = pin!(aggregate.changed.notified());
                changed.as_mut().enable();
                if aggregate.ended() {
                    return;
                }
                changed.await;
            }
        }
    }
}

impl Drop for UploadSink {
    fn drop(&mut self) {
        self.aggregate.leave();
    }
}

/// After the first accepted chunk a `progress` record falls on every multiple of this.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const HEARTBEAT_AFTER: Duration = Duration::from_secs(1);

/// One reader's lines (`api/upload.md#progress-records`), from `ready` to `complete` or an `error`, on any transport.
pub struct ProgressFeed {
    uploads: Uploads,
    aggregate: Arc<Aggregate>,
    reader: u64,
    ready: bool,
    ended: bool,
    next_tick: Option<Instant>,
    last_line: Instant,
}

enum Step {
    Wait,
    Line(Record),
    Last(Record),
    Superseded,
}

impl ProgressFeed {
    fn new(uploads: Uploads, aggregate: Arc<Aggregate>, reader: u64) -> Self {
        let next_tick = aggregate.next_tick(PROGRESS_INTERVAL);
        Self {
            uploads,
            aggregate,
            reader,
            ready: false,
            ended: false,
            next_tick,
            last_line: Instant::now(),
        }
    }

    /// The next line, a record or a heartbeat; `None` once the feed ended or a newer reader replaced it.
    pub async fn next(&mut self) -> Option<Bytes> {
        let aggregate = self.aggregate.clone();
        let mut ticked = false;
        while !self.ended {
            let mut changed = pin!(aggregate.changed.notified());
            changed.as_mut().enable();
            self.uploads.sweep();
            match self.step(ticked) {
                Step::Wait => {}
                Step::Line(record) => return Some(self.line(record.line().into())),
                Step::Last(record) => {
                    self.ended = true;
                    return Some(self.line(record.line().into()));
                }
                Step::Superseded => {
                    self.ended = true;
                    return None;
                }
            }
            // A late tick is not repeated: a stalled reader gets one fresh record, then the grid.
            self.next_tick = self
                .next_tick
                .or_else(|| aggregate.next_tick(PROGRESS_INTERVAL));
            let tick = self
                .next_tick
                .unwrap_or_else(|| Instant::now() + PROGRESS_INTERVAL);
            ticked = tokio::select! {
                biased;
                () = changed => false,
                () = sleep_until(tick) => {
                    let on_grid = self.next_tick.is_some();
                    self.next_tick = aggregate.next_tick(PROGRESS_INTERVAL);
                    on_grid
                }
                () = sleep_until(self.last_line + HEARTBEAT_AFTER) => {
                    return Some(self.line(Bytes::from_static(HEARTBEAT.as_bytes())));
                }
            };
        }
        None
    }

    fn step(&mut self, ticked: bool) -> Step {
        let life = self.aggregate.life();
        if life.reader != self.reader {
            return Step::Superseded;
        }
        if life.phase == Phase::Expired {
            return Step::Last(Refusal::Invalid.into());
        }
        if !self.ready {
            self.ready = true;
            return Step::Line(Record::Ready);
        }
        let counters = self.aggregate.counters();
        match life.phase {
            Phase::Finished if life.lanes == 0 => Step::Last(Record::Complete(counters)),
            Phase::Open if ticked && counters.nanos() > 0 => Step::Line(Record::Progress(counters)),
            Phase::Open | Phase::Finished | Phase::Expired => Step::Wait,
        }
    }

    fn line(&mut self, line: Bytes) -> Bytes {
        self.last_line = Instant::now();
        line
    }
}
