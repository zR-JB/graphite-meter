//! The stage engine (`docs/MEASUREMENTS.md`), pure and tick-synchronous: the coordinator feeds time and observations.

use crate::{
    config::MAX_WARMUP,
    format,
    measure::{
        aggregate::{Aggregate, Boundary, Reading},
        latency::{Latency, Population, ProbeOutcome, Summary},
    },
    model::{
        Cadence, Dir, Direction, Failure, LaneHealth, Scope, ServerFailure, ServerResult, Stage, StageResult,
        Throughput,
    },
};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::time::{Duration, Instant};

/// The sampler's period.
pub const TICK: Duration = Duration::from_millis(250);
const READY_BOUND: Duration = Duration::from_secs(10);
const CHECKPOINT_BUDGET: Duration = Duration::from_millis(1500);
const FINAL_CHECKPOINT_BUDGET: Duration = Duration::from_millis(500);
/// A tick this late resumes evidence.
const LATE_TICK: Duration = Duration::from_millis(1500);
/// Half a tick: a shorter window holds only a few reads, so its rates stay off the live trace.
const LIVE_WINDOW: Duration = Duration::from_millis(125);
/// Bytes that stop growing this long are silence.
const SILENCE: Duration = Duration::from_secs(2);
/// Another server that moved this recently makes a silent one's problem its own.
const QUIET: Duration = Duration::from_millis(500);
const MISSED_CHECKPOINTS: u32 = 3;
const DRAIN_BOUND: Duration = Duration::from_secs(10);
const MAX_STAGGER: Duration = Duration::from_millis(75);
const UNCHECKPOINTED: &str = "receiver checkpoint unavailable before measurement";

/// One stage for its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    pub stage: Stage,
    /// In selection order.
    pub members: Vec<Member>,
    pub duration: Duration,
    /// The probing cadence; none when the stage measures no latency.
    pub latency: Option<Cadence>,
}

/// A server in a stage, with the warmup its idle round trip asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub server: ServerId,
    pub warmup: Duration,
}

/// One member's counters and lane health at a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Its counters, with the checkpoint asked for when it arrived.
    pub reading: Reading,
    /// Its lanes are open and its upload feed advances.
    pub ready: bool,
    /// Why the checkpoint asked for did not arrive.
    pub missed: Option<Failure>,
    pub lanes: Dir<LaneHealth>,
}

/// What a member's prober observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProberEvent {
    Outcome {
        sent: Instant,
        outcome: ProbeOutcome,
    },
    /// The population failed: the channel is gone for this stage.
    Down {
        at: Instant,
        failure: Failure,
    },
    /// A channel opened; replies before it are not adjacent to those after.
    Up,
    /// Sending stopped and every probe of the window resolved.
    Drained,
}

pub struct Input<'a> {
    /// When the samples were taken.
    pub now: Instant,
    /// `lateness(fired, previous Tick.next, when the previous tick returned)`; zero for the first tick.
    pub lateness: Duration,
    pub samples: &'a [Sample],
    pub probes: &'a [(ServerId, ProberEvent)],
    /// Members whose participant could not open the stage.
    pub departed: &'a [(ServerId, Failure)],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The measured window: probes sent in it count, and it closes at `end`.
    OpenWindow { start: Instant, end: Instant },
    /// The next tick's samples carry checkpoints gathered within this budget.
    Checkpoint(Duration),
    /// The window closed: lanes stop, upload sessions finish and probers drain.
    CloseWindow,
    /// A failure the member stays for: its latency population or the stage's evidence is lost.
    Failed(ServerFailure),
    /// The member leaves the run for this failure.
    Remove(ServerFailure),
    /// The stage is over: every participant ends.
    Finish,
}

#[derive(Debug)]
pub struct Tick {
    pub decisions: Vec<Decision>,
    /// The boundary's live-trace rates: a window of at least half a tick, or any at a restart; none breaks the trace.
    pub live: Option<Dir<Option<f64>>>,
    /// When the engine wants its next tick.
    pub next: Instant,
}

#[derive(Debug)]
enum Phase {
    Ready,
    Warmup(Instant),
    /// The window opens with the next tick's checkpoints.
    Opening,
    /// `last`: the current tick's checkpoints were asked for as the final boundary's.
    Measuring {
        window: Window,
        last: bool,
    },
    Draining {
        window: Window,
        ended: Instant,
    },
    /// With the window and when it ended, once one opened, and whether the user stopped the stage.
    Done {
        measured: Option<(Window, Instant)>,
        stopped: bool,
    },
}

/// The measured window: its start, its planned end, and a transfer stage's aggregate.
#[derive(Debug)]
struct Window {
    start: Instant,
    end: Instant,
    aggregate: Option<Aggregate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probing {
    Off,
    Waiting,
    Up,
    Failed,
    Drained,
}

#[derive(Debug)]
struct Seat {
    server: ServerId,
    warmup: Duration,
    present: bool,
    probing: Probing,
    latency: Latency,
    misses: u32,
    /// When its bytes last grew, per direction.
    moved: Dir<Instant>,
}

#[derive(Debug)]
pub struct Engine {
    plan: StagePlan,
    phase: Phase,
    created: Instant,
    /// The plan's members in order.
    seats: Vec<Seat>,
    failures: Vec<ServerFailure>,
}

impl Seat {
    /// How long its bytes in `direction` have not grown.
    fn quiet(&self, direction: Direction, now: Instant) -> Duration {
        now.saturating_duration_since(self.moved[direction])
    }
}

impl Engine {
    pub fn new(plan: StagePlan, now: Instant) -> Self {
        let probing = if plan.latency.is_some() { Probing::Waiting } else { Probing::Off };
        let seat = |member: &Member| Seat {
            server: member.server.clone(),
            warmup: member.warmup,
            present: true,
            probing,
            latency: Latency::default(),
            misses: 0,
            moved: Dir { down: now, up: now },
        };
        Self {
            seats: plan.members.iter().map(seat).collect(),
            plan,
            phase: Phase::Ready,
            created: now,
            failures: Vec::new(),
        }
    }

    pub fn tick(&mut self, input: Input) -> Tick {
        let next = input.now + TICK;
        let mut tick = Tick { decisions: Vec::new(), live: None, next };
        if matches!(self.phase, Phase::Done { .. }) {
            return tick;
        }
        let survivors = self.present().count();
        for (server, failure) in input.departed {
            if let Some(index) = self.index(server) {
                self.fail(index, Scope::Server, failure.clone(), input.now, true, &mut tick.decisions);
            }
        }
        if !matches!(self.phase, Phase::Draining { .. }) {
            self.check_lanes(&input, &mut tick.decisions);
        }
        if matches!(self.phase, Phase::Ready) {
            self.ready(&input, &mut tick);
        }
        match self.phase {
            Phase::Warmup(until) if input.now >= until => self.warmed(&input, &mut tick),
            Phase::Warmup(until) => tick.next = tick.next.min(until),
            Phase::Opening => self.open(&input, &mut tick),
            Phase::Measuring { .. } => self.measure(&input, &mut tick),
            Phase::Ready | Phase::Draining { .. } | Phase::Done { .. } => {}
        }
        for (server, probe) in input.probes {
            self.probe(server, probe, &mut tick.decisions);
        }
        if matches!(self.phase, Phase::Draining { .. }) {
            self.drain(input.now);
        }
        let remaining: Vec<_> = self.present().map(|seat| seat.server.clone()).collect();
        if remaining.len() < survivors
            && let Some(aggregate) = self.aggregate_mut()
        {
            aggregate.dropout(&remaining, input.now);
        }
        if remaining.is_empty() || matches!(self.phase, Phase::Done { .. }) {
            self.finish(input.now, &mut tick.decisions);
        }
        tick
    }

    /// The user stopped the stage.
    pub fn stop(&mut self, now: Instant) {
        let measured = self.take_measured(now);
        self.phase = Phase::Done { measured, stopped: true };
    }

    /// What the stage measured; complete once it decided `Finish` or stopped.
    pub fn result(&self) -> StageResult {
        let measured = self.window().map_or(Duration::ZERO, |window| {
            let ended = self.ended().unwrap_or(window.end).min(window.end);
            ended.saturating_duration_since(window.start)
        });
        let servers = self.seats.iter().map(|seat| ServerResult {
            server: seat.server.clone(),
            left: !seat.present,
            throughput: self.throughput(Some(&seat.server)),
            latency: (seat.probing != Probing::Off).then(|| Population {
                summary: seat.latency.summary(),
                complete: seat.present && seat.probing != Probing::Failed && !self.stopped(),
            }),
        });
        StageResult {
            stage: self.plan.stage,
            measured,
            stopped: self.stopped(),
            throughput: self.throughput(None),
            servers: servers.collect(),
            failures: self.failures.clone(),
        }
    }

    /// `server`'s latency so far, counting the probes sent within the window.
    pub fn latency(&self, server: &ServerId) -> Option<Summary> {
        let seat = self.seats.iter().find(|seat| seat.server == *server)?;
        (seat.probing != Probing::Off).then(|| seat.latency.summary())
    }

    /// Per direction the stage moves, `server`'s share or all servers' headline and bytes; none without a window.
    fn throughput(&self, server: Option<&ServerId>) -> Dir<Option<Throughput>> {
        let measured = |all: &Aggregate, direction| match server {
            None => Throughput { rate: all.result(direction), bytes: all.total(direction) },
            Some(id) => Throughput {
                rate: all.server(id, direction),
                bytes: all.bytes(id, direction),
            },
        };
        let moved = |direction| self.aggregate().filter(|_| self.plan.stage.moves(direction));
        Dir::from_fn(|direction| moved(direction).map(|all| measured(all, direction)))
    }

    /// Ends the stage, recording `insufficient-evidence` for members still present whose result is missing.
    fn finish(&mut self, now: Instant, out: &mut Vec<Decision>) {
        let stopped = self.stopped();
        let measured = self.take_measured(now);
        self.phase = Phase::Done { measured, stopped };
        let lacks = |throughput: Option<Throughput>| throughput.is_some_and(|throughput| throughput.rate.is_none());
        let all = self.throughput(None);
        let explained = self.failures.iter().any(|failure| failure.scope != Scope::Latency);
        let throughput = self.window().is_some() && !explained && (lacks(all.down) || lacks(all.up));
        let latency = self.window().is_some() && self.plan.stage == Stage::Latency;
        for index in 0..self.seats.len() {
            let seat = &self.seats[index];
            let present = seat.present;
            let unmeasured = latency && seat.probing != Probing::Failed && seat.latency.summary().p50.is_none();
            let scopes = [(throughput, Scope::Throughput), (unmeasured, Scope::Latency)];
            for (_, scope) in scopes.into_iter().filter(|&(lacks, _)| lacks && present) {
                let failure = Failure::new(FailureReason::InsufficientEvidence, "too little measured time");
                self.fail(index, scope, failure, now, false, out);
            }
        }
        out.push(Decision::Finish);
    }

    /// The window and when it ended, ended `now` if it still runs; the caller sets the next phase.
    fn take_measured(&mut self, now: Instant) -> Option<(Window, Instant)> {
        match std::mem::replace(&mut self.phase, Phase::Ready) {
            Phase::Measuring { window, .. } => Some((window, now)),
            Phase::Draining { window, ended } => Some((window, ended)),
            Phase::Done { measured, .. } => measured,
            Phase::Ready | Phase::Warmup(_) | Phase::Opening => None,
        }
    }

    /// The window once it opened.
    fn window(&self) -> Option<&Window> {
        match &self.phase {
            Phase::Measuring { window, .. }
            | Phase::Draining { window, .. }
            | Phase::Done { measured: Some((window, _)), .. } => Some(window),
            Phase::Ready | Phase::Warmup(_) | Phase::Opening | Phase::Done { measured: None, .. } => None,
        }
    }

    /// The window's start and planned end once it opened.
    fn bounds(&self) -> Option<(Instant, Instant)> {
        self.window().map(|window| (window.start, window.end))
    }

    /// When the window ended, once it did.
    fn ended(&self) -> Option<Instant> {
        match self.phase {
            Phase::Draining { ended, .. } | Phase::Done { measured: Some((_, ended)), .. } => Some(ended),
            Phase::Ready
            | Phase::Warmup(_)
            | Phase::Opening
            | Phase::Measuring { .. }
            | Phase::Done { measured: None, .. } => None,
        }
    }

    fn stopped(&self) -> bool {
        matches!(self.phase, Phase::Done { stopped: true, .. })
    }

    /// The window's throughput accounting, once a transfer stage's window opened.
    pub fn aggregate(&self) -> Option<&Aggregate> {
        self.window()?.aggregate.as_ref()
    }

    fn aggregate_mut(&mut self) -> Option<&mut Aggregate> {
        match &mut self.phase {
            Phase::Measuring { window, .. }
            | Phase::Draining { window, .. }
            | Phase::Done { measured: Some((window, _)), .. } => window.aggregate.as_mut(),
            Phase::Ready | Phase::Warmup(_) | Phase::Opening | Phase::Done { measured: None, .. } => None,
        }
    }

    fn present(&self) -> impl Iterator<Item = &Seat> {
        self.seats.iter().filter(|seat| seat.present)
    }

    fn index(&self, server: &ServerId) -> Option<usize> {
        let mut seats = self.seats.iter();
        seats.position(|seat| seat.present && seat.server == *server)
    }

    /// Records a member's failure; it leaves when `leaves`.
    fn fail(&mut self, index: usize, scope: Scope, cause: Failure, at: Instant, leaves: bool, out: &mut Vec<Decision>) {
        let seat = &mut self.seats[index];
        seat.present &= !leaves;
        let failure = ServerFailure { server: seat.server.clone(), scope, failure: cause, at };
        self.failures.push(failure.clone());
        out.push(if leaves { Decision::Remove(failure) } else { Decision::Failed(failure) });
    }

    fn probe(&mut self, server: &ServerId, probe: &ProberEvent, out: &mut Vec<Decision>) {
        let Some(index) = self.index(server) else { return };
        let (window, seat) = (self.bounds(), &mut self.seats[index]);
        match probe {
            ProberEvent::Outcome { sent, outcome }
                if window.is_some_and(|(start, end)| (start..end).contains(sent)) =>
            {
                seat.latency.record(*outcome);
            }
            ProberEvent::Up => {
                seat.latency.break_continuity();
                if seat.probing == Probing::Waiting {
                    seat.probing = Probing::Up;
                }
            }
            ProberEvent::Drained if matches!(seat.probing, Probing::Waiting | Probing::Up) => {
                seat.probing = Probing::Drained
            }
            ProberEvent::Down { at, failure } => self.latency_failed(index, failure.clone(), *at, out),
            ProberEvent::Outcome { .. } | ProberEvent::Drained => {}
        }
    }

    /// A lost latency population; in the latency stage a server lost beside another leaves the run.
    fn latency_failed(&mut self, index: usize, failure: Failure, at: Instant, out: &mut Vec<Decision>) {
        if std::mem::replace(&mut self.seats[index].probing, Probing::Failed) == Probing::Failed {
            return;
        }
        let lost = matches!(failure.reason, FailureReason::ConnectionLost | FailureReason::Timeout);
        let leaves = self.plan.stage == Stage::Latency && lost && self.present().count() > 1;
        self.fail(index, Scope::Latency, failure, at, leaves, out);
    }

    /// A member whose lanes failed for good leaves, unless they failed after the window closed while it was moving.
    fn check_lanes(&mut self, input: &Input, out: &mut Vec<Decision>) {
        let directions = self.plan.stage.directions();
        let closed = self.bounds().map(|(_, end)| end).filter(|&end| input.now >= end);
        for sample in input.samples {
            let failed = directions.iter().find_map(|&direction| match &sample.lanes[direction] {
                LaneHealth::Failed(failure) => Some(failure.clone()),
                _ => None,
            });
            if let (Some(index), Some(failure)) = (self.index(&sample.reading.server), failed) {
                let seat = &self.seats[index];
                if closed.is_some_and(|end| directions.iter().all(|&direction| seat.quiet(direction, end) < QUIET)) {
                    continue;
                }
                self.fail(index, Scope::Throughput, failure, input.now, true, out);
            }
        }
    }

    /// Waits until every member's lanes and prober are ready; at the bound the late ones fail.
    fn ready(&mut self, input: &Input, tick: &mut Tick) {
        let transfers = !self.plan.stage.directions().is_empty();
        let up = |server: &ServerId| input.probes.contains(&(server.clone(), ProberEvent::Up));
        let seats = self.seats.iter().enumerate().filter(|(_, seat)| seat.present);
        let late = seats.filter_map(|(index, seat)| {
            let lanes = transfers && !sample(input, &seat.server).is_some_and(|sample| sample.ready);
            let probing = seat.probing == Probing::Waiting && !up(&seat.server);
            (lanes || probing).then_some((index, lanes))
        });
        let late: Vec<_> = late.collect();
        let bound = self.created + READY_BOUND;
        if !late.is_empty() && input.now < bound {
            tick.next = tick.next.min(bound);
            return;
        }
        let text = format!("the test server was not ready within {}", format::stage_time(READY_BOUND));
        let failure = Failure::new(FailureReason::Timeout, text);
        for (index, lanes) in late {
            match lanes {
                true => self.fail(index, Scope::Throughput, failure.clone(), input.now, true, &mut tick.decisions),
                false => self.latency_failed(index, failure.clone(), input.now, &mut tick.decisions),
            }
        }
        let warmup = self.present().map(|seat| seat.warmup).max().unwrap_or_default();
        self.phase = Phase::Warmup(input.now + warmup);
    }

    /// Uploads open the window with the next tick's checkpoints, other stages at once.
    fn warmed(&mut self, input: &Input, tick: &mut Tick) {
        if !self.plan.stage.moves(Direction::Up) {
            return self.open(input, tick);
        }
        (self.phase, tick.next) = (Phase::Opening, input.now);
        tick.decisions.push(Decision::Checkpoint(CHECKPOINT_BUDGET));
    }

    /// Opens the window from this tick's samples; an uploading member without its first checkpoint leaves.
    fn open(&mut self, input: &Input, tick: &mut Tick) {
        let (now, uploads) = (input.now, self.plan.stage.moves(Direction::Up));
        for index in 0..self.seats.len() {
            let sample = sample(input, &self.seats[index].server);
            if !uploads || !self.seats[index].present || sample.is_some_and(|sample| sample.reading.up.is_some()) {
                continue;
            }
            let refused = sample.and_then(|sample| sample.missed.clone());
            let refused = refused.filter(|refused| refused.reason == FailureReason::SignInRequired);
            let failure = refused.unwrap_or_else(|| Failure::new(FailureReason::PreparationFailed, UNCHECKPOINTED));
            self.fail(index, Scope::Throughput, failure, now, true, &mut tick.decisions);
        }
        if self.present().next().is_none() {
            return;
        }
        let end = now + self.plan.duration;
        for seat in &mut self.seats {
            seat.moved = Dir { down: now, up: now };
        }
        let aggregate = (!self.plan.stage.directions().is_empty()).then(|| {
            let participants = self.present().map(|seat| seat.server.clone()).collect();
            let mut aggregate = Aggregate::new(self.plan.stage, participants, now);
            aggregate.observe(self.boundary(input, false));
            aggregate
        });
        let window = Window { start: now, end, aggregate };
        self.phase = Phase::Measuring { window, last: false };
        tick.decisions.push(Decision::OpenWindow { start: now, end });
        self.schedule(now, tick);
    }

    /// Asks for the next boundary; the window's end is the final one.
    fn schedule(&mut self, now: Instant, tick: &mut Tick) {
        let Phase::Measuring { window, last } = &mut self.phase else { return };
        tick.next = (now + TICK).min(window.end).max(now);
        *last = tick.next >= window.end;
        if self.plan.stage.moves(Direction::Up) {
            let budget = if *last { FINAL_CHECKPOINT_BUDGET } else { CHECKPOINT_BUDGET };
            tick.decisions.push(Decision::Checkpoint(budget));
        }
    }

    fn measure(&mut self, input: &Input, tick: &mut Tick) {
        let Phase::Measuring { window, last } = &self.phase else { return };
        let checkpointed = *last || !self.plan.stage.moves(Direction::Up);
        let last = checkpointed && input.now >= window.end;
        if window.aggregate.is_some() {
            self.observe(input, last, tick);
        }
        if !last {
            return self.schedule(input.now, tick);
        }
        self.phase = match std::mem::replace(&mut self.phase, Phase::Ready) {
            Phase::Measuring { window, .. } => Phase::Draining { window, ended: input.now },
            other => other,
        };
        tick.decisions.push(Decision::CloseWindow);
    }

    /// Credits the boundary, then removes members whose checkpoints or bytes fail them.
    fn observe(&mut self, input: &Input, last: bool, tick: &mut Tick) {
        let boundary = self.boundary(input, last);
        let Phase::Measuring { window: Window { aggregate: Some(aggregate), .. }, .. } = &mut self.phase else {
            return;
        };
        let bytes = |seat: &Seat| Dir::from_fn(|direction| aggregate.bytes(&seat.server, direction));
        let before: Vec<_> = self.seats.iter().map(bytes).collect();
        let earlier = aggregate.begun();
        let window = aggregate.observe(boundary);
        let restarted = aggregate.begun() > earlier;
        if restarted || window.as_ref().is_some_and(|window| window.shortest() >= LIVE_WINDOW) {
            tick.live = Some(window.map(|window| window.rates).unwrap_or_default());
        }
        for (seat, before) in self.seats.iter_mut().zip(before) {
            for direction in Direction::BOTH {
                if aggregate.bytes(&seat.server, direction) > before[direction] {
                    seat.moved[direction] = input.now;
                }
            }
        }
        for index in 0..self.seats.len() {
            if let Some(failure) = self.departure(index, input, last) {
                self.fail(index, Scope::Throughput, failure, input.now, true, &mut tick.decisions);
            }
        }
    }

    /// Why a present member leaves at this boundary, if it does.
    fn departure(&mut self, index: usize, input: &Input, last: bool) -> Option<Failure> {
        let sample = sample(input, &self.seats[index].server);
        if !self.seats[index].present {
            return None;
        }
        if let Some(failure) = self.missed(index, sample, input.now, last) {
            return Some(failure);
        }
        // The final checkpoint can answer well after the window closed; silence counts up to the window's end.
        let closed = self.bounds().map(|(_, end)| end.min(input.now));
        let (seat, now) = (&self.seats[index], if last { closed.unwrap_or(input.now) } else { input.now });
        for &direction in self.plan.stage.directions() {
            if seat.quiet(direction, now) >= SILENCE && (last || self.moving(index, direction, now)) {
                let name = if direction == Direction::Down { "download" } else { "upload" };
                let text = format!("no {name} data for {}", format::stage_time(SILENCE));
                return Some(Failure::new(FailureReason::Timeout, text));
            }
            if let Some(LaneHealth::Retrying(failure)) = sample.map(|sample| &sample.lanes[direction])
                && last
                && seat.quiet(direction, now) >= QUIET
            {
                return Some(failure.clone());
            }
        }
        None
    }

    /// A refused checkpoint leaves at once; three missed in a row leave while another server's uploads move.
    fn missed(&mut self, index: usize, sample: Option<&Sample>, now: Instant, last: bool) -> Option<Failure> {
        let moving = self.moving(index, Direction::Up, now);
        let (seat, sample) = (&mut self.seats[index], sample?);
        if sample.reading.up.is_some() {
            seat.misses = 0;
        }
        let failure = sample.missed.as_ref()?;
        seat.misses += 1;
        let refused = failure.reason == FailureReason::SignInRequired;
        (refused || seat.misses >= MISSED_CHECKPOINTS && !last && moving).then(|| failure.clone())
    }

    /// Whether another present member's bytes in `direction` moved a moment ago.
    fn moving(&self, except: usize, direction: Direction, now: Instant) -> bool {
        let mut others = self.seats.iter().enumerate().filter(|&(index, _)| index != except);
        others.any(|(_, seat)| seat.present && seat.quiet(direction, now) < QUIET)
    }

    /// Finishes once every probing member drained, or at the drain bound.
    fn drain(&mut self, now: Instant) {
        let Phase::Draining { window, .. } = &self.phase else { return };
        let end = window.end;
        let probing = |seat: &Seat| matches!(seat.probing, Probing::Waiting | Probing::Up);
        if !self.present().any(probing) || now >= end + DRAIN_BOUND {
            let measured = self.take_measured(now);
            self.phase = Phase::Done { measured, stopped: false };
        }
    }

    fn boundary(&self, input: &Input, last: bool) -> Boundary {
        let present = self.present().filter_map(|seat| sample(input, &seat.server));
        let readings = present.map(|sample| sample.reading.clone()).collect();
        let stalled = !last && input.lateness > LATE_TICK;
        Boundary { at: input.now, stalled, last, readings }
    }
}

fn sample<'a>(input: &Input<'a>, server: &ServerId) -> Option<&'a Sample> {
    input.samples.iter().find(|sample| sample.reading.server == *server)
}

/// How late a tick fired, from its due time or the previous tick's later return, so checkpoint gathering never counts.
pub fn lateness(fired: Instant, due: Instant, returned: Instant) -> Duration {
    fired.saturating_duration_since(due.max(returned))
}

/// A server's warmup: `base` stretched to ten of its idle round trips, at most 4 s.
pub fn warmup(base: Duration, idle_rtt: Option<Duration>) -> Duration {
    base.max(idle_rtt.map_or(Duration::ZERO, |rtt| rtt.saturating_mul(10)))
        .min(MAX_WARMUP)
}

/// The spacing between lane starts: half the server's own warmup spread over its lanes, at most 75 ms.
pub fn stagger(warmup: Duration, lanes: usize) -> Duration {
    match lanes {
        0 | 1 => Duration::ZERO,
        lanes => (warmup / 2 / u32::try_from(lanes - 1).unwrap_or(u32::MAX)).min(MAX_STAGGER),
    }
}
