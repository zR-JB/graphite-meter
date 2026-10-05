//! The stage engine: one schedule owns a stage's readiness, warmup, boundaries, membership and result
//! (`docs/MEASUREMENTS.md`). It is pure and tick-synchronous: the coordinator passes time and every observation in,
//! and carries out the decisions.

use crate::{
    measure::{
        aggregate::{Aggregate, Boundary, Fed, Reading, Receiver, Window},
        latency::{Latency, Population, ProbeOutcome},
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
/// Bytes that stop growing this long are silence.
const SILENCE: Duration = Duration::from_secs(2);
/// Another server that moved this recently makes a silent one's problem its own.
const QUIET: Duration = Duration::from_millis(500);
const MISSED_CHECKPOINTS: u32 = 3;
const DRAIN_BOUND: Duration = Duration::from_secs(10);
const MAX_WARMUP: Duration = Duration::from_secs(4);
const MAX_STAGGER: Duration = Duration::from_millis(75);

/// One stage for its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    pub stage: Stage,
    /// In selection order.
    pub members: Vec<ServerId>,
    pub warmup: Duration,
    pub duration: Duration,
    /// The probing cadence; none when the stage measures no latency.
    pub latency: Option<Cadence>,
}

/// One member's local counters and lane health at a tick, with its checkpoint when one was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub server: ServerId,
    /// Its lanes are open and its upload feed advances.
    pub ready: bool,
    pub down: Option<u64>,
    pub up: Option<Result<Receiver, Failure>>,
    pub fed: Option<Fed>,
    pub lanes: Dir<LaneHealth>,
}

/// What a member's prober observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
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
    pub probes: &'a [(ServerId, Probe)],
    /// Members whose participant could not open the stage.
    pub departed: &'a [(ServerId, Failure)],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The measured window: probes sent in it count, and it closes at `end`.
    OpenWindow { start: Instant, end: Instant },
    /// The next tick's samples carry checkpoints gathered within `budget`; `last` for the final boundary.
    Checkpoint { budget: Duration, last: bool },
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
    /// The boundary's window, for live rates.
    pub window: Option<Window>,
    /// Silence every member shares: nobody leaves for it.
    pub recovering: bool,
    /// When the engine wants its next tick.
    pub next: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Warmup(Instant),
    /// The window opens with the next tick's checkpoints.
    Opening,
    Measuring,
    Draining,
    Done,
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
struct Member {
    server: ServerId,
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
    /// The measured window's start and planned end.
    window: Option<(Instant, Instant)>,
    ended: Option<Instant>,
    stopped: bool,
    /// The current tick's checkpoints were asked for as the final boundary's.
    last: bool,
    members: Vec<Member>,
    aggregate: Option<Aggregate>,
    failures: Vec<ServerFailure>,
}

impl Member {
    fn new(server: ServerId, probing: Probing, now: Instant) -> Self {
        let moved = Dir { down: now, up: now };
        Self {
            server,
            present: true,
            probing,
            latency: Latency::default(),
            misses: 0,
            moved,
        }
    }
}

impl Engine {
    pub fn new(plan: StagePlan, now: Instant) -> Self {
        let probing = if plan.latency.is_some() { Probing::Waiting } else { Probing::Off };
        let members = plan
            .members
            .iter()
            .map(|server| Member::new(server.clone(), probing, now))
            .collect();
        Self {
            plan,
            phase: Phase::Ready,
            created: now,
            window: None,
            ended: None,
            stopped: false,
            last: false,
            members,
            aggregate: None,
            failures: Vec::new(),
        }
    }

    pub fn tick(&mut self, input: Input) -> Tick {
        let next = input.now + TICK;
        let mut tick = Tick { decisions: Vec::new(), window: None, recovering: false, next };
        if self.phase == Phase::Done {
            return tick;
        }
        let survivors = self.present().count();
        for (server, failure) in input.departed {
            if let Some(index) = self.index(server) {
                self.fail(index, Scope::Server, failure.clone(), input.now, true, &mut tick.decisions);
            }
        }
        if !matches!(self.phase, Phase::Draining) {
            self.check_lanes(&input, &mut tick.decisions);
        }
        if self.phase == Phase::Ready {
            self.ready(&input, &mut tick);
        }
        match self.phase {
            Phase::Warmup(until) if input.now >= until => self.warmed(&input, &mut tick),
            Phase::Warmup(until) => tick.next = tick.next.min(until),
            Phase::Opening => self.open(&input, &mut tick),
            Phase::Measuring => self.measure(&input, &mut tick),
            _ => {}
        }
        for (server, probe) in input.probes {
            self.probe(server, probe, &mut tick.decisions);
        }
        if self.phase == Phase::Draining {
            self.drain(input.now);
        }
        let remaining: Vec<_> = self.present().map(|member| member.server.clone()).collect();
        if remaining.len() < survivors
            && let Some(aggregate) = &mut self.aggregate
        {
            aggregate.dropout(&remaining, input.now);
        }
        if remaining.is_empty() || self.phase == Phase::Done {
            self.finish(input.now, &mut tick.decisions);
        }
        tick
    }

    /// The user stopped the stage.
    pub fn stop(&mut self, now: Instant) {
        if self.window.is_some() {
            self.ended.get_or_insert(now);
        }
        (self.stopped, self.phase) = (true, Phase::Done);
    }

    /// What the stage measured; complete once it decided `Finish` or stopped.
    pub fn result(&self) -> StageResult {
        let measured = self.window.map_or(Duration::ZERO, |(start, end)| {
            let ended = self.ended.unwrap_or(end).min(end);
            ended.saturating_duration_since(start)
        });
        let servers = self.members.iter().map(|member| ServerResult {
            server: member.server.clone(),
            left: !member.present,
            throughput: self.throughput(Some(&member.server)),
            latency: (member.probing != Probing::Off).then(|| Population {
                summary: member.latency.summary(),
                complete: member.present && member.probing != Probing::Failed && !self.stopped,
            }),
        });
        let (intervals, omitted) = self.aggregate.as_ref().map_or((Vec::new(), 0), |aggregate| {
            let (intervals, omitted) = aggregate.intervals();
            (intervals.iter().cloned().collect(), omitted)
        });
        StageResult {
            stage: self.plan.stage,
            measured,
            stopped: self.stopped,
            throughput: self.throughput(None),
            servers: servers.collect(),
            failures: self.failures.clone(),
            intervals,
            omitted,
        }
    }

    /// Per direction the stage moves, `server`'s share or all servers' headline and bytes.
    fn throughput(&self, server: Option<&ServerId>) -> Dir<Option<Throughput>> {
        let measured = |direction| {
            let Some(aggregate) = &self.aggregate else {
                return Throughput { rate: None, bytes: 0 };
            };
            match server {
                Some(server) => Throughput {
                    rate: aggregate.server(server, direction),
                    bytes: aggregate.bytes(server, direction),
                },
                None => Throughput {
                    rate: aggregate.result(direction),
                    bytes: aggregate.total(direction),
                },
            }
        };
        Dir::from_fn(|direction| self.plan.stage.moves(direction).then(|| measured(direction)))
    }

    /// Ends the stage, recording `insufficient-evidence` for members still present whose result is missing.
    fn finish(&mut self, now: Instant, decisions: &mut Vec<Decision>) {
        self.ended.get_or_insert(now);
        self.phase = Phase::Done;
        if self.window.is_some() {
            let headless = self.plan.stage.directions().iter().any(|&direction| {
                let aggregate = self.aggregate.as_ref();
                aggregate.is_some_and(|aggregate| aggregate.result(direction).is_none())
            });
            let throughput = headless && self.failures.iter().all(|failure| failure.scope == Scope::Latency);
            for index in 0..self.members.len() {
                let member = &self.members[index];
                let unmeasured = member.probing != Probing::Failed && member.latency.summary().p50.is_none();
                let latency = self.plan.stage == Stage::Latency && unmeasured;
                let present = member.present;
                for (_, scope) in [(throughput, Scope::Throughput), (latency, Scope::Latency)]
                    .into_iter()
                    .filter(|&(lacks, _)| lacks && present)
                {
                    let failure = Failure::new(FailureReason::InsufficientEvidence, "too little measured time");
                    self.fail(index, scope, failure, now, false, decisions);
                }
            }
        }
        decisions.push(Decision::Finish);
    }

    fn present(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().filter(|member| member.present)
    }

    fn index(&self, server: &ServerId) -> Option<usize> {
        self.members
            .iter()
            .position(|member| member.present && member.server == *server)
    }

    /// Records a member's failure, removing it when `remove`.
    fn fail(
        &mut self,
        index: usize,
        scope: Scope,
        failure: Failure,
        at: Instant,
        remove: bool,
        decisions: &mut Vec<Decision>,
    ) {
        let member = &mut self.members[index];
        member.present &= !remove;
        let failure = ServerFailure { server: member.server.clone(), scope, failure, at };
        self.failures.push(failure.clone());
        decisions.push(if remove { Decision::Remove(failure) } else { Decision::Failed(failure) });
    }

    fn probe(&mut self, server: &ServerId, probe: &Probe, decisions: &mut Vec<Decision>) {
        let Some(index) = self.index(server) else { return };
        let (window, member) = (self.window, &mut self.members[index]);
        match probe {
            Probe::Outcome { sent, outcome } if window.is_some_and(|(start, end)| (start..end).contains(sent)) => {
                member.latency.record(*outcome);
            }
            Probe::Outcome { .. } => {}
            Probe::Up => {
                member.latency.break_continuity();
                if member.probing == Probing::Waiting {
                    member.probing = Probing::Up;
                }
            }
            Probe::Drained if matches!(member.probing, Probing::Waiting | Probing::Up) => {
                member.probing = Probing::Drained
            }
            Probe::Drained => {}
            Probe::Down { at, failure } => self.latency_failed(index, failure.clone(), *at, decisions),
        }
    }

    /// A lost latency population; in the latency stage a server lost beside another leaves the run.
    fn latency_failed(&mut self, index: usize, failure: Failure, at: Instant, decisions: &mut Vec<Decision>) {
        if std::mem::replace(&mut self.members[index].probing, Probing::Failed) == Probing::Failed {
            return;
        }
        let lost = matches!(failure.reason, FailureReason::ConnectionLost | FailureReason::Timeout);
        let remove = self.plan.stage == Stage::Latency && lost && self.present().count() > 1;
        self.fail(index, Scope::Latency, failure, at, remove, decisions);
    }

    /// A member whose lanes failed for good leaves.
    fn check_lanes(&mut self, input: &Input, decisions: &mut Vec<Decision>) {
        for sample in input.samples {
            let Some(index) = self.index(&sample.server) else { continue };
            let failed = self
                .plan
                .stage
                .directions()
                .iter()
                .find_map(|&direction| match &sample.lanes[direction] {
                    LaneHealth::Failed(failure) => Some(failure.clone()),
                    _ => None,
                });
            if let Some(failure) = failed {
                self.fail(index, Scope::Throughput, failure, input.now, true, decisions);
            }
        }
    }

    /// Waits until every member's lanes and prober are ready; at the bound the late ones fail.
    fn ready(&mut self, input: &Input, tick: &mut Tick) {
        let transfers = !self.plan.stage.directions().is_empty();
        let ready = |server: &ServerId| {
            let mut samples = input.samples.iter();
            samples.any(|sample| sample.server == *server && sample.ready)
        };
        let up = |server: &ServerId| {
            input
                .probes
                .iter()
                .any(|(id, probe)| id == server && *probe == Probe::Up)
        };
        let members = self.members.iter().enumerate().filter(|(_, member)| member.present);
        let late = members.filter_map(|(index, member)| {
            let lanes = transfers && !ready(&member.server);
            let probing = member.probing == Probing::Waiting && !up(&member.server);
            (lanes || probing).then_some((index, lanes))
        });
        let late: Vec<_> = late.collect();
        let bound = self.created + READY_BOUND;
        if !late.is_empty() && input.now < bound {
            tick.next = tick.next.min(bound);
            return;
        }
        let failure = Failure::new(FailureReason::Timeout, "server resources were not ready within 10 seconds");
        for (index, lanes) in late {
            match lanes {
                true => self.fail(index, Scope::Throughput, failure.clone(), input.now, true, &mut tick.decisions),
                false => self.latency_failed(index, failure.clone(), input.now, &mut tick.decisions),
            }
        }
        self.phase = Phase::Warmup(input.now + self.plan.warmup);
    }

    /// Uploads open the window with the next tick's checkpoints, other stages at once.
    fn warmed(&mut self, input: &Input, tick: &mut Tick) {
        if !self.plan.stage.moves(Direction::Up) {
            return self.open(input, tick);
        }
        (self.phase, tick.next) = (Phase::Opening, input.now);
        tick.decisions
            .push(Decision::Checkpoint { budget: CHECKPOINT_BUDGET, last: false });
    }

    /// Opens the window from this tick's samples; a member without its first checkpoint leaves.
    fn open(&mut self, input: &Input, tick: &mut Tick) {
        let now = input.now;
        for index in 0..self.members.len() {
            let member = &self.members[index];
            let sample = input.samples.iter().find(|sample| sample.server == member.server);
            let failure = match sample.and_then(|sample| sample.up.as_ref()) {
                _ if !member.present || !self.plan.stage.moves(Direction::Up) => continue,
                Some(Ok(_)) => continue,
                Some(Err(failure)) if failure.reason == FailureReason::SignInRequired => failure.clone(),
                _ => {
                    Failure::new(FailureReason::PreparationFailed, "receiver checkpoint unavailable before measurement")
                }
            };
            self.fail(index, Scope::Throughput, failure, now, true, &mut tick.decisions);
        }
        self.window = Some((now, now + self.plan.duration));
        for member in &mut self.members {
            member.moved = Dir { down: now, up: now };
        }
        if !self.plan.stage.directions().is_empty() {
            let participants = self.present().map(|member| member.server.clone()).collect();
            let mut aggregate = Aggregate::new(self.plan.stage, participants, now);
            aggregate.observe(self.boundary(input, false));
            self.aggregate = Some(aggregate);
        }
        self.phase = Phase::Measuring;
        tick.decisions
            .push(Decision::OpenWindow { start: now, end: now + self.plan.duration });
        self.schedule(now, tick);
    }

    /// Asks for the next boundary; the window's end is the final one.
    fn schedule(&mut self, now: Instant, tick: &mut Tick) {
        let Some((_, end)) = self.window else { return };
        tick.next = (now + TICK).min(end).max(now);
        self.last = tick.next >= end;
        if self.plan.stage.moves(Direction::Up) {
            let budget = if self.last { FINAL_CHECKPOINT_BUDGET } else { CHECKPOINT_BUDGET };
            tick.decisions.push(Decision::Checkpoint { budget, last: self.last });
        }
    }

    fn measure(&mut self, input: &Input, tick: &mut Tick) {
        let checkpointed = self.last || !self.plan.stage.moves(Direction::Up);
        let last = checkpointed && self.window.is_some_and(|(_, end)| input.now >= end);
        if self.aggregate.is_some() {
            self.observe(input, last, tick);
        }
        if !last {
            return self.schedule(input.now, tick);
        }
        (self.ended, self.phase) = (Some(input.now), Phase::Draining);
        tick.decisions.push(Decision::CloseWindow);
    }

    /// Credits the boundary, then removes members whose checkpoints or bytes fail them.
    fn observe(&mut self, input: &Input, last: bool, tick: &mut Tick) {
        let boundary = self.boundary(input, last);
        let Some(aggregate) = &mut self.aggregate else { return };
        let bytes = |member: &Member| Dir::from_fn(|direction| aggregate.bytes(&member.server, direction));
        let before: Vec<_> = self.members.iter().map(bytes).collect();
        tick.window = aggregate.observe(boundary);
        for (member, before) in self.members.iter_mut().zip(before) {
            for direction in Direction::BOTH {
                if aggregate.bytes(&member.server, direction) > before[direction] {
                    member.moved[direction] = input.now;
                }
            }
        }
        for index in 0..self.members.len() {
            if let Some(failure) = self.departure(index, input, last) {
                self.fail(index, Scope::Throughput, failure, input.now, true, &mut tick.decisions);
            }
        }
        let shared = |direction| self.present().all(|member| self.silent(member, direction, input.now));
        tick.recovering = !last && self.plan.stage.directions().iter().any(|&direction| shared(direction));
    }

    /// Why a present member leaves at this boundary, if it does.
    fn departure(&mut self, index: usize, input: &Input, last: bool) -> Option<Failure> {
        let member = &self.members[index];
        let sample = input.samples.iter().find(|sample| sample.server == member.server);
        if !member.present {
            return None;
        }
        if let Some(failure) = self.missed(index, sample, input.now, last) {
            return Some(failure);
        }
        let member = &self.members[index];
        for &direction in self.plan.stage.directions() {
            if self.silent(member, direction, input.now) && (last || self.moving(index, direction, input.now)) {
                let name = if direction == Direction::Down { "download" } else { "upload" };
                return Some(Failure::new(FailureReason::Timeout, format!("{name} bytes stopped growing for 2s")));
            }
            let quiet = input.now.saturating_duration_since(member.moved[direction]) >= QUIET;
            if let Some(LaneHealth::Retrying(failure)) = sample.map(|sample| &sample.lanes[direction])
                && last
                && quiet
            {
                return Some(failure.clone());
            }
        }
        None
    }

    /// A refused checkpoint leaves at once; three missed in a row leave while another server's uploads move.
    fn missed(&mut self, index: usize, sample: Option<&Sample>, now: Instant, last: bool) -> Option<Failure> {
        let moving = self.moving(index, Direction::Up, now);
        let member = &mut self.members[index];
        match sample?.up.as_ref()? {
            Ok(_) => {
                member.misses = 0;
                None
            }
            Err(failure) => {
                member.misses += 1;
                let refused = failure.reason == FailureReason::SignInRequired;
                (refused || member.misses >= MISSED_CHECKPOINTS && !last && moving).then(|| failure.clone())
            }
        }
    }

    /// Whether another present member's bytes in `direction` moved a moment ago.
    fn moving(&self, except: usize, direction: Direction, now: Instant) -> bool {
        let recent = |member: &Member| now.saturating_duration_since(member.moved[direction]) < QUIET;
        let mut members = self.members.iter().enumerate();
        members.any(|(index, member)| index != except && member.present && recent(member))
    }

    /// Whether the member's bytes in `direction` stopped growing for the silence limit.
    fn silent(&self, member: &Member, direction: Direction, now: Instant) -> bool {
        now.saturating_duration_since(member.moved[direction]) >= SILENCE
    }

    /// Finishes once every probing member drained, or at the drain bound.
    fn drain(&mut self, now: Instant) {
        let end = self.window.map_or(now, |(_, end)| end);
        let probing = self
            .present()
            .any(|member| matches!(member.probing, Probing::Waiting | Probing::Up));
        if !probing || now >= end + DRAIN_BOUND {
            self.phase = Phase::Done;
        }
    }

    fn boundary(&self, input: &Input, last: bool) -> Boundary {
        let present = input
            .samples
            .iter()
            .filter(|sample| self.index(&sample.server).is_some());
        let reading = |sample: &Sample| Reading {
            server: sample.server.clone(),
            down: sample.down,
            up: sample.up.as_ref().and_then(|up| up.as_ref().ok().copied()),
            fed: sample.fed,
        };
        Boundary {
            at: input.now,
            stalled: !last && input.lateness > LATE_TICK,
            last,
            readings: present.map(reading).collect(),
        }
    }
}

/// How late a tick's timer fired: from when it was due, or from when the previous tick returned if that was later, so
/// time spent gathering checkpoints never counts.
pub fn lateness(fired: Instant, due: Instant, returned: Instant) -> Duration {
    fired.saturating_duration_since(due.max(returned))
}

/// The warmup before a stage: `base` stretched to ten idle round trips of the slowest server, at most 4 s.
pub fn warmup(base: Duration, idle_rtts: &[Duration]) -> Duration {
    let stretched = idle_rtts
        .iter()
        .fold(base, |warmup, rtt| warmup.max(rtt.saturating_mul(10)));
    stretched.min(MAX_WARMUP)
}

/// The spacing between lane starts: half the warmup spread over the lanes, at most 75 ms.
pub fn stagger(warmup: Duration, lanes: usize) -> Duration {
    match lanes {
        0 | 1 => Duration::ZERO,
        lanes => (warmup / 2 / u32::try_from(lanes - 1).unwrap_or(u32::MAX)).min(MAX_STAGGER),
    }
}
