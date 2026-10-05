//! One stage's coordinated throughput: boundaries give windows, intervals, the headline, the peak and byte ledgers
//! (`docs/MEASUREMENTS.md`, `api/aggregation.testvectors.json`).

use crate::model::{Dir, Direction, Stage};
use graphite_meter_proto::{catalog::ServerId, upload::Counters};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

/// A headline needs this much time on the client's clock and on every receiver's.
const MIN_EVIDENCE: Duration = Duration::from_millis(800);
/// A peak window spans at least this much on every clock.
const MIN_PEAK: Duration = Duration::from_millis(500);
/// A stage keeps its latest this many intervals.
pub const MAX_INTERVALS: usize = 128;

/// One upload receiver's counters; a replacement receiver has a higher `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receiver {
    pub id: u32,
    pub counters: Counters,
}

/// The highest byte count a receiver's progress feed reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fed {
    pub id: u32,
    pub bytes: u64,
}

/// One server's counters at a boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub server: ServerId,
    /// Payload bytes the client consumed.
    pub down: Option<u64>,
    /// A fresh receiver checkpoint.
    pub up: Option<Receiver>,
    pub fed: Option<Fed>,
}

/// Every server's counters at one instant of the client's clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    pub at: Instant,
    /// The sampler ran late, so evidence resumes here.
    pub stalled: bool,
    /// The stage's final boundary.
    pub last: bool,
    pub readings: Vec<Reading>,
}

impl Boundary {
    fn reading(&self, server: &ServerId) -> Option<&Reading> {
        self.readings.iter().find(|reading| reading.server == *server)
    }
}

/// Why an interval began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    StageStart,
    Dropout,
    EvidenceResumed,
}

impl Reason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::StageStart => "stage-start",
            Self::Dropout => "dropout",
            Self::EvidenceResumed => "evidence-resumed",
        }
    }
}

/// One server's share of a window in one direction; uploads run on the receiver's clock.
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub server: ServerId,
    pub bytes: u64,
    pub duration: Duration,
    pub rate: f64,
}

/// The combined rates between two boundaries, in bytes per second.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub start: Instant,
    pub end: Instant,
    pub components: Dir<Vec<Component>>,
    pub rates: Dir<Option<f64>>,
}

impl Window {
    fn all(&self) -> impl Iterator<Item = &Component> {
        self.components.down.iter().chain(&self.components.up)
    }

    /// The shortest clock the window spans.
    pub fn shortest(&self) -> Duration {
        let receivers = self.components.up.iter().map(|component| component.duration);
        receivers.fold(self.end - self.start, Duration::min)
    }
}

/// A stretch of fixed membership whose evidence is one window.
#[derive(Debug, Clone, PartialEq)]
pub struct Interval {
    pub reason: Reason,
    pub participants: Vec<ServerId>,
    pub start: Instant,
    pub end: Instant,
    /// False once evidence resumed after it; then it no longer counts.
    pub complete: bool,
    pub window: Option<Window>,
    peaks: Peaks,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Peaks {
    combined: Dir<f64>,
    /// Per participant, in the interval's order.
    servers: Vec<Dir<f64>>,
}

impl Peaks {
    fn record(&mut self, window: &Window) {
        for direction in Direction::BOTH {
            let (components, combined) = (&window.components[direction], &mut self.combined[direction]);
            *combined = window.rates[direction].map_or(*combined, |rate| combined.max(rate));
            let len = self.servers.len().max(components.len());
            self.servers.resize(len, Dir::default());
            for (peak, component) in self.servers.iter_mut().zip(components) {
                peak[direction] = peak[direction].max(component.rate);
            }
        }
    }
}

/// A headline: the mean and peak in bytes per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub mean: f64,
    pub peak: f64,
}

/// The current interval as of a boundary where a server moved, restored when that server departs.
#[derive(Debug)]
struct Mark {
    boundary: Boundary,
    window: Option<Window>,
    peaks: Peaks,
}

impl Mark {
    fn of(boundary: &Boundary, interval: &Interval) -> Arc<Self> {
        let (window, peaks) = (interval.window.clone(), interval.peaks.clone());
        Arc::new(Self { boundary: boundary.clone(), window, peaks })
    }
}

/// The current interval's evidence once its first boundary is in.
#[derive(Debug)]
struct Open {
    first: Boundary,
    last: Boundary,
    peak_from: Boundary,
    /// Per participant in order and direction, the latest boundary where its bytes moved.
    moved: Vec<Dir<Option<Arc<Mark>>>>,
}

/// Unique measured bytes of one server, whatever window counts.
#[derive(Debug)]
struct Ledger {
    server: ServerId,
    down: Option<u64>,
    /// The current receiver's highest count.
    up: Option<Fed>,
    bytes: Dir<u64>,
}

/// One stage's accounting.
#[derive(Debug)]
pub struct Aggregate {
    stage: Stage,
    intervals: VecDeque<Interval>,
    omitted: usize,
    ledgers: Vec<Ledger>,
    open: Option<Open>,
    latest: Option<Boundary>,
}

enum Gap {
    /// No time passed on some clock: the boundary adds nothing.
    Stale,
    /// A counter regressed or a receiver was replaced: evidence resumes.
    Broken,
}

impl Aggregate {
    pub fn new(stage: Stage, participants: Vec<ServerId>, at: Instant) -> Self {
        let ledger = |server: &ServerId| Ledger {
            server: server.clone(),
            down: None,
            up: None,
            bytes: Dir::default(),
        };
        let ledgers = participants.iter().map(ledger).collect();
        let mut aggregate = Self {
            stage,
            intervals: VecDeque::new(),
            omitted: 0,
            ledgers,
            open: None,
            latest: None,
        };
        aggregate.restart(Reason::StageStart, participants, at);
        aggregate
    }

    /// The stage's intervals, the latest last, and how many older ones were dropped.
    pub fn intervals(&self) -> (&VecDeque<Interval>, usize) {
        (&self.intervals, self.omitted)
    }

    /// Credits `boundary` and returns the window since the previous one, if it adds one.
    pub fn observe(&mut self, boundary: Boundary) -> Option<Window> {
        self.credit(&boundary);
        self.latest = Some(boundary.clone());
        if self.open.is_some() && boundary.stalled {
            return self.resume(boundary);
        }
        let interval = self.intervals.back()?;
        let (down, up) = (self.stage.moves(Direction::Down), self.stage.moves(Direction::Up));
        let complete = |reading: Option<&Reading>| {
            reading.is_some_and(|reading| (!down || reading.down.is_some()) && (!up || reading.up.is_some()))
        };
        if interval.participants.is_empty() || !interval.participants.iter().all(|id| complete(boundary.reading(id))) {
            return None;
        }
        let Some(open) = &self.open else {
            self.start(boundary);
            return None;
        };
        let sample = match self.window(&open.last, &boundary) {
            Err(Gap::Broken) => return self.resume(boundary),
            Ok(sample) if !boundary.last || sample.all().all(|component| component.bytes > 0) => sample,
            _ => return None,
        };
        let Ok(full) = self.window(&open.first, &boundary) else {
            return self.resume(boundary);
        };
        let peak = self.window(&open.peak_from, &boundary).ok();
        let peak = peak.filter(|window| window.shortest() >= MIN_PEAK);
        let interval = self.intervals.back_mut()?;
        let open = self.open.as_mut()?;
        interval.end = boundary.at;
        interval.window = Some(full);
        if let Some(peak) = &peak {
            interval.peaks.record(peak);
            open.peak_from = boundary.clone();
        }
        let mark = Mark::of(&boundary, interval);
        for direction in Direction::BOTH {
            for (moved, component) in open.moved.iter_mut().zip(&sample.components[direction]) {
                if component.bytes > 0 {
                    moved[direction] = Some(mark.clone());
                }
            }
        }
        open.last = boundary;
        Some(sample)
    }

    /// Departed servers leave: the interval ends where the first of them last moved, and the survivors' dropout
    /// interval starts there.
    pub fn dropout(&mut self, survivors: &[ServerId], at: Instant) {
        let Some(interval) = self.intervals.back() else { return };
        if survivors.len() == interval.participants.len() {
            return;
        }
        let moved = self.open.as_ref().map_or(&[][..], |open| &open.moved);
        let paired = interval.participants.iter().zip(moved);
        let departed = paired.filter(|(id, _)| !survivors.contains(id));
        let marks = departed.flat_map(|(_, moved)| [&moved.down, &moved.up]).flatten();
        let Some(end) = marks.min_by_key(|mark| mark.boundary.at).cloned() else {
            if !survivors.is_empty() {
                self.restart(Reason::Dropout, survivors.to_vec(), at);
            }
            return;
        };
        if let Some(interval) = self.intervals.back_mut() {
            interval.end = end.boundary.at;
            interval.window.clone_from(&end.window);
            interval.peaks.clone_from(&end.peaks);
        }
        if survivors.is_empty() {
            return;
        }
        let latest = self.latest.clone();
        self.restart(Reason::Dropout, survivors.to_vec(), end.boundary.at);
        self.start(end.boundary.clone());
        if let Some(latest) = latest.filter(|latest| latest.at > end.boundary.at) {
            self.observe(latest);
        }
    }

    /// The headline: the latest counted interval with enough evidence in `direction`.
    pub fn result(&self, direction: Direction) -> Option<Rate> {
        self.counted().find_map(|(interval, window)| {
            let enough = interval.end - interval.start >= MIN_EVIDENCE && evidence(&window.components[direction]);
            let mean = window.rates[direction].filter(|_| enough)?;
            Some(Rate { mean, peak: interval.peaks.combined[direction].max(mean) })
        })
    }

    /// `server`'s own headline: its component in the latest counted interval where it has enough evidence.
    pub fn server(&self, server: &ServerId, direction: Direction) -> Option<Rate> {
        self.counted().find_map(|(interval, window)| {
            let index = interval.participants.iter().position(|id| id == server)?;
            let (component, peaks) = (window.components[direction].get(index)?, interval.peaks.servers.get(index));
            let (mean, peak) = (component.rate, peaks.map_or(0.0, |peaks| peaks[direction]));
            evidence(std::slice::from_ref(component)).then_some(Rate { mean, peak: peak.max(mean) })
        })
    }

    /// `server`'s unique measured bytes in `direction`.
    pub fn bytes(&self, server: &ServerId, direction: Direction) -> u64 {
        let ledger = self.ledgers.iter().find(|ledger| ledger.server == *server);
        ledger.map_or(0, |ledger| ledger.bytes[direction])
    }

    pub fn total(&self, direction: Direction) -> u64 {
        let bytes = self.ledgers.iter().map(|ledger| ledger.bytes[direction]);
        bytes.fold(0, u64::saturating_add)
    }

    /// Complete intervals with a window, the latest first.
    fn counted(&self) -> impl Iterator<Item = (&Interval, &Window)> {
        let complete = self.intervals.iter().rev().filter(|interval| interval.complete);
        complete.filter_map(|interval| Some((interval, interval.window.as_ref()?)))
    }

    fn restart(&mut self, reason: Reason, participants: Vec<ServerId>, at: Instant) {
        if self.intervals.len() == MAX_INTERVALS {
            self.intervals.pop_front();
            self.omitted += 1;
        }
        let peaks = Peaks::default();
        let interval = Interval {
            reason,
            participants,
            start: at,
            end: at,
            complete: true,
            window: None,
            peaks,
        };
        self.intervals.push_back(interval);
        self.open = None;
    }

    fn start(&mut self, boundary: Boundary) {
        let Some(interval) = self.intervals.back_mut() else { return };
        (interval.start, interval.end) = (boundary.at, boundary.at);
        let mark = Mark::of(&boundary, interval);
        let moved = Dir::from_fn(|direction| self.stage.moves(direction).then(|| mark.clone()));
        let moved = vec![moved; interval.participants.len()];
        self.open = Some(Open {
            first: boundary.clone(),
            last: boundary.clone(),
            peak_from: boundary,
            moved,
        });
    }

    /// Closes the interval as no longer counting and starts an evidence-resumed one at `boundary`.
    fn resume(&mut self, boundary: Boundary) -> Option<Window> {
        let interval = self.intervals.back_mut()?;
        interval.complete = false;
        let participants = interval.participants.clone();
        self.restart(Reason::EvidenceResumed, participants, boundary.at);
        self.observe(boundary)
    }

    fn window(&self, first: &Boundary, last: &Boundary) -> Result<Window, Gap> {
        let elapsed = last.at.saturating_duration_since(first.at);
        if elapsed.is_zero() {
            return Err(Gap::Stale);
        }
        let participants = self.intervals.back().map_or(&[][..], |interval| &interval.participants);
        let (mut components, mut stale) = (Dir::<Vec<Component>>::default(), false);
        for id in participants {
            let (start, end) = first.reading(id).zip(last.reading(id)).ok_or(Gap::Broken)?;
            let component = |bytes: u64, duration: Duration| {
                let rate = bytes as f64 / duration.as_secs_f64();
                Component { server: id.clone(), bytes, duration, rate }
            };
            if self.stage.moves(Direction::Down) {
                let (start, end) = start.down.zip(end.down).ok_or(Gap::Broken)?;
                let bytes = end.checked_sub(start).ok_or(Gap::Broken)?;
                components.down.push(component(bytes, elapsed));
            }
            if self.stage.moves(Direction::Up) {
                let (start, end) = start.up.zip(end.up).ok_or(Gap::Broken)?;
                if start.id != end.id || !end.counters.follows(start.counters) {
                    return Err(Gap::Broken);
                }
                let (start, end) = (start.counters, end.counters);
                let (bytes, nanos) = (end.bytes() - start.bytes(), end.nanos() - start.nanos());
                stale |= nanos == 0;
                components.up.push(component(bytes, Duration::from_nanos(nanos)));
            }
        }
        if stale {
            return Err(Gap::Stale);
        }
        let total = |components: &Vec<Component>| {
            (!components.is_empty()).then(|| components.iter().map(|component| component.rate).sum())
        };
        let rates = Dir { down: total(&components.down), up: total(&components.up) };
        Ok(Window { start: first.at, end: last.at, components, rates })
    }

    fn credit(&mut self, boundary: &Boundary) {
        for reading in &boundary.readings {
            let Some(ledger) = self.ledgers.iter_mut().find(|ledger| ledger.server == reading.server) else {
                continue;
            };
            if let Some(count) = reading.down {
                let high = ledger.down.get_or_insert(count);
                ledger.bytes.down = ledger.bytes.down.saturating_add(count.saturating_sub(*high));
                *high = (*high).max(count);
            }
            let checkpoint = reading.up.map(|up| Fed { id: up.id, bytes: up.counters.bytes() });
            for fed in reading.fed.into_iter().chain(checkpoint) {
                ledger.credit(fed);
            }
        }
    }
}

impl Ledger {
    /// Credits the current receiver's growth and a newer receiver's whole count.
    fn credit(&mut self, next: Fed) {
        let added = match self.up {
            None => 0,
            Some(current) if next.id < current.id || next.id == current.id && next.bytes <= current.bytes => return,
            Some(current) if next.id == current.id => next.bytes - current.bytes,
            Some(_) => next.bytes,
        };
        self.bytes.up = self.bytes.up.saturating_add(added);
        self.up = Some(next);
    }
}

/// Every component spans enough time and some moved bytes.
fn evidence(components: &[Component]) -> bool {
    !components.is_empty()
        && components.iter().all(|component| component.duration >= MIN_EVIDENCE)
        && components.iter().any(|component| component.bytes > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> ServerId {
        ServerId::parse(text).unwrap()
    }

    fn downloads(at: Instant, counts: &[(&str, u64)]) -> Boundary {
        let reading =
            |&(server, count): &(&str, u64)| Reading { server: id(server), down: Some(count), up: None, fed: None };
        Boundary {
            at,
            stalled: false,
            last: false,
            readings: counts.iter().map(reading).collect(),
        }
    }

    #[test]
    fn a_server_that_never_moved_ends_the_interval_at_its_first_boundary() {
        let base = Instant::now();
        let ms = |ms| base + Duration::from_millis(ms);
        let mut aggregate = Aggregate::new(Stage::Download, vec![id("a"), id("b")], base);
        for (at, a) in [(0, 0), (500, 500), (1000, 1000), (1500, 1500)] {
            aggregate.observe(downloads(ms(at), &[("a", a), ("b", 0)]));
        }
        aggregate.dropout(&[id("a")], ms(1600));
        aggregate.observe(downloads(ms(2000), &[("a", 2000)]));
        let (intervals, _) = aggregate.intervals();
        let [first, dropout] = [&intervals[0], &intervals[1]];
        assert_eq!((first.start, first.end, first.window.is_none()), (base, base, true));
        assert_eq!((dropout.reason, dropout.participants.as_slice()), (Reason::Dropout, &[id("a")][..]));
        let window = dropout.window.as_ref().unwrap();
        assert_eq!((window.start, window.end, window.rates.down), (base, ms(2000), Some(1000.0)));
        assert_eq!(aggregate.result(Direction::Down), Some(Rate { mean: 1000.0, peak: 1000.0 }));
    }

    #[test]
    fn a_feed_still_naming_a_replaced_receiver_adds_nothing() {
        let base = Instant::now();
        let mut aggregate = Aggregate::new(Stage::Upload, vec![id("a")], base);
        let ticks = [
            ((0, 1000), (0, 1000, 1)),
            ((0, 1500), (1, 200, 1)),
            ((0, 1500), (1, 400, 2)),
            ((1, 600), (1, 500, 3)),
        ];
        for (tick, ((fed_id, fed), (up_id, bytes, nanos))) in (0..).zip(ticks) {
            let reading = Reading {
                server: id("a"),
                down: None,
                up: Some(Receiver { id: up_id, counters: Counters::new(bytes, nanos) }),
                fed: Some(Fed { id: fed_id, bytes: fed }),
            };
            let at = base + Duration::from_millis(250 * tick);
            aggregate.observe(Boundary { at, stalled: false, last: false, readings: vec![reading] });
        }
        assert_eq!(aggregate.total(Direction::Up), 500 + 200 + 200 + 200);
    }

    #[test]
    fn a_stage_keeps_its_latest_intervals() {
        let base = Instant::now();
        let mut aggregate = Aggregate::new(Stage::Download, vec![id("a")], base);
        for tick in 0..MAX_INTERVALS as u64 + 2 {
            let mut boundary = downloads(base + Duration::from_secs(tick), &[("a", tick)]);
            boundary.stalled = true;
            aggregate.observe(boundary);
        }
        let (intervals, omitted) = aggregate.intervals();
        assert_eq!((intervals.len(), omitted), (MAX_INTERVALS, 2));
        assert!(
            intervals
                .iter()
                .all(|interval| interval.reason == Reason::EvidenceResumed)
        );
    }
}
