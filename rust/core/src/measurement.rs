//! Single-owner aggregate measurement accounting across coordinated servers.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

pub const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
pub const CHECKPOINT_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);
pub const FINAL_CHECKPOINT_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);
pub const CLIENT_STALL: std::time::Duration = std::time::Duration::from_millis(1500);

pub const MIN_SURVIVOR_NANOS: u64 = 800_000_000;
const MIN_PEAK_NANOS: u64 = 500_000_000;
pub const MAX_INTERVALS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Download,
    Upload,
    Bidirectional,
}

impl Stage {
    pub fn needs_down(self) -> bool {
        self != Self::Upload
    }
    pub fn needs_up(self) -> bool {
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

impl IntervalReason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::StageStart => "stage-start",
            Self::Dropout => "dropout",
            Self::EvidenceResumed => "evidence-resumed",
        }
    }
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
    pub stalled: bool,
    pub final_boundary: bool,
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

impl AggregateWindow {
    fn direction(&self, direction: Direction) -> (&[ComponentWindow], Option<f64>) {
        match direction {
            Direction::Down => (&self.down, self.down_bytes_per_sec),
            Direction::Up => (&self.up, self.up_bytes_per_sec),
        }
    }

    fn shortest(&self) -> u64 {
        self.up
            .iter()
            .map(|component| component.duration_nanos)
            .fold(self.end_nanos - self.start_nanos, u64::min)
    }
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
    stats: WindowStats,
}

#[derive(Debug, Clone, Default)]
struct WindowStats {
    samples: usize,
    peak: [f64; 2],
    server_peaks: BTreeMap<String, [f64; 2]>,
}

impl WindowStats {
    fn record_peak(&mut self, window: &AggregateWindow) {
        for direction in [Direction::Down, Direction::Up] {
            let (components, rate) = window.direction(direction);
            if let Some(rate) = rate {
                self.peak[direction as usize] = self.peak[direction as usize].max(rate);
            }
            for component in components {
                let peak = &mut self.server_peaks.entry(component.server_id.clone()).or_default()[direction as usize];
                *peak = peak.max(component.bytes_per_sec);
            }
        }
    }
}

/// An interval as of one boundary, restored when a departing server last moved there.
#[derive(Debug)]
struct Mark {
    boundary: Boundary,
    window: Option<AggregateWindow>,
    stats: WindowStats,
}

#[derive(Debug, Clone)]
pub struct MeasurementResult {
    pub direction: Direction,
    pub total_bytes: u64,
    pub mean_bytes_per_sec: Option<f64>,
    pub peak_bytes_per_sec: Option<f64>,
    pub samples: usize,
    pub elapsed_nanos: Option<u64>,
}

impl MeasurementResult {
    fn unavailable(direction: Direction, total_bytes: u64) -> Self {
        Self {
            direction,
            total_bytes,
            mean_bytes_per_sec: None,
            peak_bytes_per_sec: None,
            samples: 0,
            elapsed_nanos: None,
        }
    }
}

#[derive(Debug, Default)]
struct Ledger {
    down: Option<u64>,
    upload: Option<ObservedUpload>,
    bytes: [u64; 2],
}

#[derive(Debug, Default)]
pub struct AggregateMeasurements {
    intervals: VecDeque<AggregationInterval>,
    omitted_intervals: usize,
    first: Option<Boundary>,
    last: Option<Boundary>,
    peak_from: Option<Boundary>,
    latest: Option<Boundary>,
    moved: BTreeMap<String, [Option<Arc<Mark>>; 2]>,
    servers: BTreeMap<String, Ledger>,
}

impl AggregateMeasurements {
    pub fn intervals(&self) -> &VecDeque<AggregationInterval> {
        &self.intervals
    }
    pub fn omitted_intervals(&self) -> usize {
        self.omitted_intervals
    }

    pub fn begin_stage(&mut self, stage: Stage, participants: Vec<String>, at_nanos: u64) {
        self.servers = participants.iter().map(|id| (id.clone(), Ledger::default())).collect();
        self.latest = None;
        self.restart(stage, participants, at_nanos, IntervalReason::StageStart);
    }

    fn restart(&mut self, stage: Stage, participants: Vec<String>, at_nanos: u64, reason: IntervalReason) {
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
            stats: WindowStats::default(),
        });
        self.first = None;
        self.last = None;
        self.peak_from = None;
        self.moved.clear();
    }

    pub fn observe(&mut self, boundary: Boundary) -> Option<AggregateWindow> {
        self.intervals.back()?;
        self.credit(&boundary);
        self.latest = Some(boundary.clone());
        let interval = self.intervals.back()?;
        if self.first.is_some() && boundary.stalled {
            return self.resume(boundary);
        }
        if interval.participants.is_empty()
            || interval.participants.iter().any(|id| {
                interval.stage.needs_down() && !boundary.down.contains_key(id)
                    || interval.stage.needs_up() && !boundary.up.contains_key(id)
            })
        {
            return None;
        }
        let (Some(first), Some(last)) = (&self.first, &self.last) else {
            self.start(boundary);
            return None;
        };
        if boundary.final_boundary
            && interval.participants.iter().any(|id| {
                interval.stage.needs_down() && boundary.down[id] <= last.down[id]
                    || interval.stage.needs_up()
                        && boundary.up[id].id == last.up[id].id
                        && boundary.up[id].bytes <= last.up[id].bytes
            })
        {
            return None;
        }
        let sample = window(last, &boundary, interval);
        if matches!(sample, Err(Gap::Stale)) {
            return None;
        }
        let (Ok(sample), Ok(full)) = (sample, window(first, &boundary, interval)) else {
            return self.resume(boundary);
        };
        let peak = window(self.peak_from.as_ref().unwrap(), &boundary, interval)
            .ok()
            .filter(|window| window.shortest() >= MIN_PEAK_NANOS);
        let interval = self.intervals.back_mut().unwrap();
        interval.end_nanos = boundary.at_nanos;
        interval.window = Some(full);
        interval.stats.samples += 1;
        if let Some(peak) = &peak {
            interval.stats.record_peak(peak);
            self.peak_from = Some(boundary.clone());
        }
        let mark = Arc::new(Mark {
            boundary: boundary.clone(),
            window: interval.window.clone(),
            stats: interval.stats.clone(),
        });
        for direction in [Direction::Down, Direction::Up] {
            for component in sample
                .direction(direction)
                .0
                .iter()
                .filter(|component| component.bytes > 0)
            {
                if let Some(moved) = self.moved.get_mut(&component.server_id) {
                    moved[direction as usize] = Some(mark.clone());
                }
            }
        }
        self.last = Some(boundary);
        Some(sample)
    }

    fn start(&mut self, boundary: Boundary) {
        let interval = self.intervals.back_mut().unwrap();
        (interval.start_nanos, interval.end_nanos) = (boundary.at_nanos, boundary.at_nanos);
        let mark = Arc::new(Mark {
            boundary: boundary.clone(),
            window: interval.window.clone(),
            stats: interval.stats.clone(),
        });
        let moved = [interval.stage.needs_down(), interval.stage.needs_up()].map(|needed| needed.then(|| mark.clone()));
        self.moved = interval
            .participants
            .iter()
            .map(|id| (id.clone(), moved.clone()))
            .collect();
        self.first = Some(boundary.clone());
        self.peak_from = Some(boundary.clone());
        self.last = Some(boundary);
    }

    fn resume(&mut self, boundary: Boundary) -> Option<AggregateWindow> {
        let interval = self.intervals.back_mut().unwrap();
        interval.complete = false;
        let (stage, participants) = (interval.stage, interval.participants.clone());
        self.restart(stage, participants, boundary.at_nanos, IntervalReason::EvidenceResumed);
        self.observe(boundary)
    }

    pub fn dropout(&mut self, survivors: &[String], at_nanos: u64) {
        let Some(interval) = self.intervals.back() else {
            return;
        };
        if survivors.len() == interval.participants.len() {
            return;
        }
        let stage = interval.stage;
        let end = self
            .moved
            .iter()
            .filter(|(id, _)| !survivors.contains(id))
            .flat_map(|(_, moved)| moved.iter().flatten())
            .min_by_key(|mark| mark.boundary.at_nanos)
            .cloned();
        let Some(end) = end else {
            if !survivors.is_empty() {
                self.restart(stage, survivors.to_vec(), at_nanos, IntervalReason::Dropout);
            }
            return;
        };
        let interval = self.intervals.back_mut().unwrap();
        interval.end_nanos = end.boundary.at_nanos;
        interval.window.clone_from(&end.window);
        interval.stats.clone_from(&end.stats);
        if survivors.is_empty() {
            return;
        }
        let latest = self.latest.clone();
        self.restart(
            stage,
            survivors.to_vec(),
            end.boundary.at_nanos,
            IntervalReason::Dropout,
        );
        self.start(end.boundary.clone());
        if let Some(latest) = latest.filter(|latest| latest.at_nanos > end.boundary.at_nanos) {
            self.observe(latest);
        }
    }

    pub fn result(&self, direction: Direction) -> MeasurementResult {
        let total = self.servers.values().map(|server| server.bytes[direction as usize]);
        let mut result = MeasurementResult::unavailable(direction, total.fold(0, u64::saturating_add));
        for interval in self.stage_intervals() {
            let Some(window) = interval.window.as_ref().filter(|_| interval.complete) else {
                continue;
            };
            let (components, rate) = window.direction(direction);
            if interval.end_nanos - interval.start_nanos < MIN_SURVIVOR_NANOS || !evidence(components) {
                continue;
            }
            result.mean_bytes_per_sec = rate;
            result.peak_bytes_per_sec = rate.map(|rate| interval.stats.peak[direction as usize].max(rate));
            result.samples = interval.stats.samples;
            result.elapsed_nanos = Some(match direction {
                Direction::Down => interval.end_nanos - interval.start_nanos,
                Direction::Up => components
                    .iter()
                    .map(|component| component.duration_nanos)
                    .max()
                    .unwrap(),
            });
            return result;
        }
        result
    }

    pub fn server_result(&self, id: &str, direction: Direction) -> MeasurementResult {
        let total = self
            .servers
            .get(id)
            .map_or(0, |server| server.bytes[direction as usize]);
        let mut result = MeasurementResult::unavailable(direction, total);
        for interval in self.stage_intervals() {
            let Some(window) = interval.window.as_ref().filter(|_| interval.complete) else {
                continue;
            };
            let Some(component) = window
                .direction(direction)
                .0
                .iter()
                .find(|component| component.server_id == id && evidence(std::slice::from_ref(component)))
            else {
                continue;
            };
            let peak = interval
                .stats
                .server_peaks
                .get(id)
                .map_or(0.0, |peak| peak[direction as usize]);
            result.mean_bytes_per_sec = Some(component.bytes_per_sec);
            result.peak_bytes_per_sec = Some(peak.max(component.bytes_per_sec));
            result.samples = interval.stats.samples;
            result.elapsed_nanos = Some(component.duration_nanos);
            return result;
        }
        result
    }

    fn stage_intervals(&self) -> impl Iterator<Item = &AggregationInterval> {
        let stage = self.intervals.back().map(|interval| interval.stage);
        self.intervals
            .iter()
            .rev()
            .take_while(move |interval| Some(interval.stage) == stage)
    }

    fn credit(&mut self, boundary: &Boundary) {
        for (id, &count) in &boundary.down {
            let Some(server) = self.servers.get_mut(id) else {
                continue;
            };
            match server.down {
                Some(previous) if count < previous => continue,
                Some(previous) => server.bytes[0] = server.bytes[0].saturating_add(count - previous),
                None => {}
            }
            server.down = Some(count);
        }
        for (id, observed) in &boundary.observed_up {
            if boundary
                .up
                .get(id)
                .is_none_or(|snapshot| snapshot.id != observed.id || snapshot.bytes < observed.maximum)
            {
                self.credit_upload(id, observed.clone());
            }
        }
        for (id, snapshot) in &boundary.up {
            self.credit_upload(
                id,
                ObservedUpload {
                    id: snapshot.id.clone(),
                    maximum: snapshot.bytes,
                },
            );
        }
    }

    fn credit_upload(&mut self, id: &str, next: ObservedUpload) {
        let Some(server) = self.servers.get_mut(id) else {
            return;
        };
        let credit = match &server.upload {
            None => 0,
            Some(previous) if previous.id != next.id => next.maximum,
            Some(previous) if next.maximum <= previous.maximum => return,
            Some(previous) => next.maximum - previous.maximum,
        };
        server.bytes[1] = server.bytes[1].saturating_add(credit);
        server.upload = Some(next);
    }
}

fn evidence(components: &[ComponentWindow]) -> bool {
    !components.is_empty()
        && components
            .iter()
            .all(|component| component.duration_nanos >= MIN_SURVIVOR_NANOS)
        && components.iter().any(|component| component.bytes > 0)
}

enum Gap {
    Stale,
    Invalid,
}

fn window(first: &Boundary, last: &Boundary, interval: &AggregationInterval) -> Result<AggregateWindow, Gap> {
    let elapsed = last.at_nanos.saturating_sub(first.at_nanos);
    if elapsed == 0 {
        return Err(Gap::Stale);
    }
    let mut window = AggregateWindow {
        start_nanos: first.at_nanos,
        end_nanos: last.at_nanos,
        down: Vec::new(),
        up: Vec::new(),
        down_bytes_per_sec: None,
        up_bytes_per_sec: None,
    };
    let mut stale = false;
    for id in &interval.participants {
        if interval.stage.needs_down() {
            let (Some(&start), Some(&end)) = (first.down.get(id), last.down.get(id)) else {
                return Err(Gap::Invalid);
            };
            let bytes = end.checked_sub(start).ok_or(Gap::Invalid)?;
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
            let (Some(start), Some(end)) = (first.up.get(id), last.up.get(id)) else {
                return Err(Gap::Invalid);
            };
            if start.id != end.id || end.bytes < start.bytes || end.nanos < start.nanos {
                return Err(Gap::Invalid);
            }
            if end.nanos == start.nanos {
                stale = true;
                continue;
            }
            let (bytes, duration) = (end.bytes - start.bytes, end.nanos - start.nanos);
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
    if stale {
        return Err(Gap::Stale);
    }
    Ok(window)
}
