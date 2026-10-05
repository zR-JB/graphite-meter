//! A progress reader's feed (`api/upload.md#progress-records`), the same lines on every transport.

use super::uploads::{Aggregate, Uploads};
use bytes::Bytes;
use graphite_meter_proto::{
    refusal::UploadRefusal,
    upload::{HEARTBEAT, Record},
};
use std::{pin::pin, sync::Arc, time::Duration};
use tokio::time::{Instant, sleep_until};

/// After the first accepted chunk a `progress` record follows this often.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// A blank line follows this long after the feed's last line.
pub const HEARTBEAT_AFTER: Duration = Duration::from_secs(1);

/// The lines one reader receives, from `ready` to `complete` or an `error`.
pub struct ProgressFeed {
    uploads: Uploads,
    aggregate: Arc<Aggregate>,
    reader: u64,
    ready: bool,
    ended: bool,
    next_tick: Instant,
    last_line: Instant,
}

/// What the aggregate's state calls for.
enum Step {
    Wait,
    Line(Record),
    Last(Record),
    Superseded,
}

impl ProgressFeed {
    pub(super) fn new(uploads: Uploads, aggregate: Arc<Aggregate>, reader: u64) -> Self {
        let now = Instant::now();
        Self {
            uploads,
            aggregate,
            reader,
            ready: false,
            ended: false,
            next_tick: now + PROGRESS_INTERVAL,
            last_line: now,
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
            ticked = tokio::select! {
                () = changed => false,
                () = sleep_until(self.next_tick) => {
                    self.next_tick = Instant::now() + PROGRESS_INTERVAL;
                    true
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
        if life.expired {
            return Step::Last(UploadRefusal::Invalid.into());
        }
        if !self.ready {
            self.ready = true;
            return Step::Line(Record::Ready);
        }
        let counters = self.aggregate.counters();
        match life.finished {
            true if life.lanes == 0 => Step::Last(Record::Complete(counters)),
            false if ticked && counters.nanos() > 0 => Step::Line(Record::Progress(counters)),
            _ => Step::Wait,
        }
    }

    fn line(&mut self, line: Bytes) -> Bytes {
        self.last_line = Instant::now();
        line
    }
}
