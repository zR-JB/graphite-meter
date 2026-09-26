//! Single-owner aggregate measurement accounting across coordinated servers.

use std::collections::{BTreeMap, VecDeque};

pub const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
pub const CHECKPOINT_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);
pub const FINAL_CHECKPOINT_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);
pub const MAX_CHECKPOINT_GAP: std::time::Duration =
    SAMPLE_INTERVAL.saturating_add(CHECKPOINT_BUDGET);

pub const MIN_SURVIVOR_NANOS: u64 = 800_000_000;
pub const MAX_INTERVALS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Download,
    Upload,
    Bidirectional,
}

impl Stage {
    fn needs_down(self) -> bool {
        self != Self::Upload
    }
    fn needs_up(self) -> bool {
        self != Self::Download
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalReason {
    StageStart,
    Dropout,
    EvidenceResumed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverSnapshot {
    pub id: String,
    pub bytes: u64,
    pub nanos: u64,
    pub requested_at_nanos: u64,
    pub received_at_nanos: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedUpload {
    pub id: String,
    pub maximum: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Boundary {
    pub at_nanos: u64,
    pub down: BTreeMap<String, u64>,
    pub up: BTreeMap<String, ReceiverSnapshot>,
    pub observed_up: BTreeMap<String, ObservedUpload>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    ClientMonotonic,
    Receiver,
}

#[derive(Debug, Clone)]
pub struct ComponentWindow {
    pub server_id: String,
    pub bytes: u64,
    pub duration_nanos: u64,
    pub bytes_per_sec: f64,
    pub clock: Clock,
    pub start_bytes: u64,
    pub end_bytes: u64,
    pub start_receiver: Option<ReceiverSnapshot>,
    pub end_receiver: Option<ReceiverSnapshot>,
}

#[derive(Debug, Clone)]
pub struct AggregateWindow {
    pub start_nanos: u64,
    pub end_nanos: u64,
    pub down: Vec<ComponentWindow>,
    pub up: Vec<ComponentWindow>,
    pub down_bytes_per_sec: Option<f64>,
    pub up_bytes_per_sec: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AggregationInterval {
    pub id: usize,
    pub stage: Stage,
    pub participants: Vec<String>,
    pub start_nanos: u64,
    pub end_nanos: u64,
    pub complete: bool,
    pub reason: IntervalReason,
    pub window: Option<AggregateWindow>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ByteTotals {
    pub down: u64,
    pub up: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    SurvivorEvidence,
    ComponentEvidence,
}

#[derive(Debug, Clone)]
pub struct MeasurementResult {
    pub stage: Stage,
    pub direction: Direction,
    pub total_bytes: u64,
    pub mean_bytes_per_sec: Option<f64>,
    pub peak_bytes_per_sec: Option<f64>,
    pub samples: usize,
    pub elapsed_nanos: Option<u64>,
    pub unavailable_reason: Option<UnavailableReason>,
}

#[derive(Debug, Default)]
pub struct AggregateMeasurements {
    intervals: VecDeque<AggregationInterval>,
    omitted_intervals: usize,
    first: Option<Boundary>,
    last: Option<Boundary>,
    down_peak: f64,
    up_peak: f64,
    samples: usize,
    totals: BTreeMap<String, ByteTotals>,
    stage_totals: BTreeMap<Stage, BTreeMap<String, ByteTotals>>,
    uploads: BTreeMap<String, ObservedUpload>,
    down_seen: BTreeMap<String, u64>,
    stage: Option<Stage>,
}

impl AggregateMeasurements {
    pub fn intervals(&self) -> &VecDeque<AggregationInterval> {
        &self.intervals
    }
    pub fn omitted_intervals(&self) -> usize {
        self.omitted_intervals
    }
    pub fn total_for_server(&self, id: &str) -> ByteTotals {
        self.totals.get(id).copied().unwrap_or_default()
    }
    pub fn stage_total_for_server(&self, stage: Stage, id: &str) -> ByteTotals {
        self.stage_totals
            .get(&stage)
            .and_then(|totals| totals.get(id))
            .copied()
            .unwrap_or_default()
    }

    pub fn begin(
        &mut self,
        stage: Stage,
        participants: Vec<String>,
        at_nanos: u64,
        reason: IntervalReason,
    ) {
        if reason == IntervalReason::StageStart {
            self.uploads.clear();
            self.down_seen.clear();
        }
        self.stage = Some(stage);
        self.stage_totals.entry(stage).or_default();
        if self.intervals.len() == MAX_INTERVALS {
            self.intervals.pop_front();
            self.omitted_intervals += 1;
        }
        self.intervals.push_back(AggregationInterval {
            id: self.omitted_intervals + self.intervals.len(),
            stage,
            participants,
            start_nanos: at_nanos,
            end_nanos: at_nanos,
            complete: true,
            reason,
            window: None,
        });
        self.first = None;
        self.last = None;
        self.down_peak = 0.0;
        self.up_peak = 0.0;
        self.samples = 0;
    }

    pub fn observe(&mut self, boundary: Boundary) -> Option<AggregateWindow> {
        self.intervals.back()?;
        self.ledger(&boundary);
        let interval = self.intervals.back()?;
        let valid = !interval.participants.is_empty()
            && interval.participants.iter().all(|id| {
                (!interval.stage.needs_down() || boundary.down.contains_key(id))
                    && (!interval.stage.needs_up() || boundary.up.contains_key(id))
            });
        let gap_too_large = self.last.as_ref().is_some_and(|last| {
            boundary.at_nanos.saturating_sub(last.at_nanos) > MAX_CHECKPOINT_GAP.as_nanos() as u64
        });
        if gap_too_large {
            self.intervals.back_mut().unwrap().complete = false;
        }
        if !valid {
            return None;
        }
        if !self.intervals.back().unwrap().complete {
            return self.resume_at(boundary);
        }
        if self.first.is_none() {
            let interval = self.intervals.back_mut().unwrap();
            interval.start_nanos = boundary.at_nanos;
            interval.end_nanos = boundary.at_nanos;
            self.first = Some(boundary.clone());
            self.last = Some(boundary);
            return None;
        }
        let interval = self.intervals.back().unwrap();
        let sample = aggregate_window(self.last.as_ref().unwrap(), &boundary, interval);
        let full = aggregate_window(self.first.as_ref().unwrap(), &boundary, interval);
        let (Some(sample), Some(full)) = (sample, full) else {
            let interval = self.intervals.back_mut().unwrap();
            interval.complete = false;
            interval.end_nanos = boundary.at_nanos;
            return self.resume_at(boundary);
        };
        self.last = Some(boundary.clone());
        let interval = self.intervals.back_mut().unwrap();
        interval.end_nanos = boundary.at_nanos;
        interval.window = Some(full);
        self.samples += 1;
        if let Some(rate) = sample.down_bytes_per_sec {
            self.down_peak = self.down_peak.max(rate);
        }
        if let Some(rate) = sample.up_bytes_per_sec {
            self.up_peak = self.up_peak.max(rate);
        }
        Some(sample)
    }

    pub fn result(&self, stage: Stage, direction: Direction) -> MeasurementResult {
        let total_bytes = self
            .stage_totals
            .get(&stage)
            .into_iter()
            .flat_map(|m| m.values())
            .map(|total| match direction {
                Direction::Down => total.down,
                Direction::Up => total.up,
            })
            .fold(0u64, u64::saturating_add);
        let mut result = MeasurementResult {
            stage,
            direction,
            total_bytes,
            mean_bytes_per_sec: None,
            peak_bytes_per_sec: None,
            samples: 0,
            elapsed_nanos: None,
            unavailable_reason: Some(UnavailableReason::SurvivorEvidence),
        };
        let Some(interval) = self.intervals.back() else {
            return result;
        };
        if interval.stage != stage || !interval.complete {
            return result;
        }
        let Some(window) = &interval.window else {
            return result;
        };
        let Some(elapsed) = interval.end_nanos.checked_sub(interval.start_nanos) else {
            return result;
        };
        if elapsed < MIN_SURVIVOR_NANOS {
            return result;
        }
        let (components, rate, peak) = match direction {
            Direction::Down => (&window.down, window.down_bytes_per_sec, self.down_peak),
            Direction::Up => (&window.up, window.up_bytes_per_sec, self.up_peak),
        };
        if rate.is_none()
            || components.is_empty()
            || components
                .iter()
                .any(|component| component.duration_nanos < MIN_SURVIVOR_NANOS)
        {
            result.unavailable_reason = Some(UnavailableReason::ComponentEvidence);
            return result;
        }
        result.mean_bytes_per_sec = rate;
        result.peak_bytes_per_sec = Some(peak);
        result.samples = self.samples;
        result.elapsed_nanos = Some(elapsed);
        result.unavailable_reason = None;
        result
    }

    /// A peer's rate is valid only when the coordinated survivor window is
    /// valid. Its lifetime byte total is available separately even if this
    /// rate is unavailable or the peer dropped out of the final interval.
    pub fn server_rate(&self, stage: Stage, direction: Direction, id: &str) -> Option<f64> {
        self.result(stage, direction).mean_bytes_per_sec?;
        let window = self.intervals.back()?.window.as_ref()?;
        let components = match direction {
            Direction::Down => &window.down,
            Direction::Up => &window.up,
        };
        components
            .iter()
            .find(|component| component.server_id == id)
            .map(|component| component.bytes_per_sec)
    }

    fn resume_at(&mut self, boundary: Boundary) -> Option<AggregateWindow> {
        let interval = self.intervals.back().unwrap();
        let (stage, participants) = (interval.stage, interval.participants.clone());
        self.begin(
            stage,
            participants,
            boundary.at_nanos,
            IntervalReason::EvidenceResumed,
        );
        // The ledger was already updated before the gap was discovered.
        self.observe(boundary)
    }

    fn credit(&mut self, id: &str, direction: Direction, bytes: u64) {
        let stage = self.stage.expect("begin precedes observation");
        for total in [
            self.totals.entry(id.to_owned()).or_default(),
            self.stage_totals
                .entry(stage)
                .or_default()
                .entry(id.to_owned())
                .or_default(),
        ] {
            let counter = match direction {
                Direction::Down => &mut total.down,
                Direction::Up => &mut total.up,
            };
            *counter = counter.saturating_add(bytes);
        }
    }

    fn ledger(&mut self, boundary: &Boundary) {
        for (id, &count) in &boundary.down {
            match self.down_seen.get(id).copied() {
                Some(previous) if count < previous => continue,
                Some(previous) => self.credit(id, Direction::Down, count - previous),
                None => {}
            }
            self.down_seen.insert(id.clone(), count);
        }
        for (id, observation) in &boundary.observed_up {
            if boundary.up.get(id).is_some_and(|snapshot| {
                snapshot.id == observation.id && snapshot.bytes >= observation.maximum
            }) {
                continue;
            }
            self.credit_upload(id, observation);
        }
        for (id, snapshot) in &boundary.up {
            self.credit_upload(
                id,
                &ObservedUpload {
                    id: snapshot.id.clone(),
                    maximum: snapshot.bytes,
                },
            );
        }
    }

    fn credit_upload(&mut self, id: &str, observation: &ObservedUpload) {
        if let Some(previous) = self.uploads.get(id) {
            if previous.id == observation.id && observation.maximum <= previous.maximum {
                return;
            }
            let baseline = if previous.id == observation.id {
                previous.maximum
            } else {
                0
            };
            if observation.maximum >= baseline {
                self.credit(id, Direction::Up, observation.maximum - baseline);
            }
        }
        self.uploads.insert(id.to_owned(), observation.clone());
    }
}

fn aggregate_window(
    first: &Boundary,
    last: &Boundary,
    interval: &AggregationInterval,
) -> Option<AggregateWindow> {
    let elapsed = last.at_nanos.checked_sub(first.at_nanos)?;
    if elapsed == 0 || elapsed > i64::MAX as u64 {
        return None;
    }
    let mut window = AggregateWindow {
        start_nanos: first.at_nanos,
        end_nanos: last.at_nanos,
        down: Vec::new(),
        up: Vec::new(),
        down_bytes_per_sec: None,
        up_bytes_per_sec: None,
    };
    for id in &interval.participants {
        if interval.stage.needs_down() {
            let start = *first.down.get(id)?;
            let end = *last.down.get(id)?;
            let bytes = end.checked_sub(start)?;
            let rate = bytes as f64 / (elapsed as f64 / 1e9);
            window.down.push(ComponentWindow {
                server_id: id.clone(),
                bytes,
                duration_nanos: elapsed,
                bytes_per_sec: rate,
                clock: Clock::ClientMonotonic,
                start_bytes: start,
                end_bytes: end,
                start_receiver: None,
                end_receiver: None,
            });
            *window.down_bytes_per_sec.get_or_insert(0.0) += rate;
        }
        if interval.stage.needs_up() {
            let start = first.up.get(id)?;
            let end = last.up.get(id)?;
            if start.id != end.id {
                return None;
            }
            let bytes = end.bytes.checked_sub(start.bytes)?;
            let duration = end.nanos.checked_sub(start.nanos)?;
            if duration == 0 || duration > i64::MAX as u64 {
                return None;
            }
            let rate = bytes as f64 / (duration as f64 / 1e9);
            window.up.push(ComponentWindow {
                server_id: id.clone(),
                bytes,
                duration_nanos: duration,
                bytes_per_sec: rate,
                clock: Clock::Receiver,
                start_bytes: start.bytes,
                end_bytes: end.bytes,
                start_receiver: Some(start.clone()),
                end_receiver: Some(end.clone()),
            });
            *window.up_bytes_per_sec.get_or_insert(0.0) += rate;
        }
    }
    Some(window)
}
