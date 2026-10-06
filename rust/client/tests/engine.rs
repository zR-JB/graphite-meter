//! The stage engine against scripted timelines.

use graphite_meter_client::{
    measure::{
        aggregate::{Fed, Reading, Reason, Receiver},
        latency::ProbeOutcome,
    },
    model::{Cadence, Dir, Failure, LaneHealth, Outcome, Scope, ServerFailure, Stage, StageResult, StageStatus, focus},
    run::engine::{Decision, Engine, Input, Member, Probe, Sample, StagePlan, lateness},
};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason, upload::Counters};
use std::time::{Duration, Instant};

const fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

const STAGE: Duration = ms(10_000);

fn id(text: &str) -> ServerId {
    ServerId::parse(text).unwrap()
}

fn plan(stage: Stage, members: &[&str], latency: bool) -> StagePlan {
    StagePlan {
        stage,
        members: members
            .iter()
            .map(|member| Member { server: id(member), warmup: Duration::ZERO })
            .collect(),
        duration: STAGE,
        latency: latency.then_some(Cadence::ReplyDriven),
    }
}

/// Bytes of a server moving one kilobyte per millisecond until `stop`.
fn moved(at: Duration, stop: Duration) -> u64 {
    at.min(stop).as_millis() as u64 * 1000
}

fn sample(server: &str) -> Sample {
    let reading = Reading { server: id(server), down: None, up: None, fed: None };
    let lanes = Dir { down: LaneHealth::Ok, up: LaneHealth::Ok };
    Sample { reading, ready: true, missed: None, lanes }
}

fn with(mut sample: Sample, change: impl FnOnce(&mut Reading)) -> Sample {
    change(&mut sample.reading);
    sample
}

fn down(server: &str, bytes: u64) -> Sample {
    with(sample(server), |reading| reading.down = Some(bytes))
}

fn checkpoint(server: &str, bytes: u64, nanos: u64) -> Sample {
    let counters = Counters::new(bytes, nanos);
    with(sample(server), |reading| reading.up = Some(Receiver { id: 0, counters }))
}

/// A checkpoint whose receiver clock runs with the client's.
fn up(server: &str, bytes: u64, at: Duration) -> Sample {
    checkpoint(server, bytes, at.as_nanos() as u64 + 1)
}

/// An engine ticked at scripted times, with what it decided.
struct Script {
    engine: Engine,
    base: Instant,
    next: Duration,
    log: Vec<(Duration, Decision)>,
    recovering: Vec<Duration>,
    /// When the live trace got rates.
    live: Vec<Duration>,
    /// Departures the next tick passes in.
    departed: Vec<(ServerId, Failure)>,
}

impl Script {
    fn new(plan: StagePlan) -> Self {
        let base = Instant::now();
        let engine = Engine::new(plan, base);
        Self {
            engine,
            base,
            next: Duration::ZERO,
            log: Vec::new(),
            recovering: Vec::new(),
            live: Vec::new(),
            departed: Vec::new(),
        }
    }

    fn tick(&mut self, at: Duration, lateness: Duration, samples: &[Sample], probes: &[(ServerId, Probe)]) {
        let (now, departed) = (self.base + at, std::mem::take(&mut self.departed));
        let tick = self
            .engine
            .tick(Input { now, lateness, samples, probes, departed: &departed });
        self.next = tick.next - self.base;
        self.log
            .extend(tick.decisions.into_iter().map(|decision| (at, decision)));
        if tick.recovering {
            self.recovering.push(at);
        }
        if tick.live.is_some() {
            self.live.push(at);
        }
    }

    /// A tick on time.
    fn step(&mut self, at: Duration, samples: &[Sample], probes: &[(ServerId, Probe)]) {
        self.tick(at, Duration::ZERO, samples, probes);
    }

    /// Ticks when the engine asks, through `until` or its finish.
    fn run_until(&mut self, until: Duration, samples: impl Fn(Duration) -> Vec<Sample>) {
        while !self.finished() && self.next <= until {
            let at = self.next;
            self.step(at, &samples(at), &[]);
        }
    }

    fn run(&mut self, samples: impl Fn(Duration) -> Vec<Sample>) {
        self.run_until(STAGE * 3, samples);
        assert!(self.finished());
    }

    fn finished(&self) -> bool {
        self.log.iter().any(|(_, decision)| *decision == Decision::Finish)
    }

    fn at(&self, wanted: &Decision) -> Option<Duration> {
        let found = self.log.iter().find(|(_, decision)| decision == wanted);
        found.map(|(at, _)| *at)
    }

    /// When the window opened and when it closes.
    fn window(&self) -> Option<(Duration, Duration)> {
        self.log.iter().find_map(|(_, decision)| match decision {
            Decision::OpenWindow { start, end } => Some((*start - self.base, *end - self.base)),
            _ => None,
        })
    }

    fn removed(&self) -> Vec<(Duration, &str, FailureReason)> {
        let removals = self.log.iter().filter_map(|(at, decision)| match decision {
            Decision::Remove(failure) => Some((*at, failure.server.as_str(), failure.failure.reason)),
            _ => None,
        });
        removals.collect()
    }

    /// Failures the members stayed for.
    fn failed(&self) -> Vec<(Duration, &str, Scope, FailureReason)> {
        let failures = self.log.iter().filter_map(|(at, decision)| match decision {
            Decision::Failed(failure) => Some((*at, failure.server.as_str(), failure.scope, failure.failure.reason)),
            _ => None,
        });
        failures.collect()
    }
}

#[test]
fn an_upload_server_falls_silent_when_its_ledger_stops_growing_on_the_client_clock() {
    let mut script = Script::new(plan(Stage::Upload, &["a"], false));
    script.run(|at| vec![up("a", moved(at, ms(7000)), at.min(ms(7000)))]);
    assert_eq!(script.removed(), [(STAGE, "a", FailureReason::Timeout)]);

    let mut script = Script::new(plan(Stage::Upload, &["a", "b"], false));
    script.run(|at| vec![up("a", moved(at, STAGE), at), checkpoint("b", 0, 0)]);
    assert_eq!(script.removed(), [(ms(2000), "b", FailureReason::Timeout)]);

    let mut script = Script::new(plan(Stage::Upload, &["a", "b"], false));
    script.run(|at| {
        let fed = Fed { id: 0, bytes: moved(at, STAGE) };
        vec![
            up("a", moved(at, STAGE), at),
            with(checkpoint("b", 1000, 1), |reading| reading.fed = Some(fed)),
        ]
    });
    assert!(script.removed().is_empty());
}

fn intervals(result: &StageResult) -> Vec<(Reason, bool)> {
    let intervals = result.intervals.iter();
    intervals.map(|interval| (interval.reason, interval.complete)).collect()
}

#[test]
fn a_late_timer_resumes_evidence_and_a_slow_checkpoint_does_not() {
    // The tick at 3 s wants the next at 3.25 s and returns at once, or at 4.5 s after slow checkpoints.
    for (stage, fired, returned, resumed) in
        [(Stage::Download, ms(4800), ms(3000), true), (Stage::Upload, ms(4500), ms(4500), false)]
    {
        let mut script = Script::new(plan(stage, &["a"], false));
        let samples =
            |at: Duration| vec![with(up("a", moved(at, STAGE), at), |reading| reading.down = Some(moved(at, STAGE)))];
        script.run_until(ms(3000), samples);
        assert_eq!(script.next, ms(3250));
        let at = |offset| script.base + offset;
        let late = lateness(at(fired), at(script.next), at(returned));
        script.tick(fired, late, &samples(fired), &[]);
        script.run(samples);
        let expected = match resumed {
            true => vec![(Reason::StageStart, false), (Reason::EvidenceResumed, true)],
            false => vec![(Reason::StageStart, true)],
        };
        assert_eq!(intervals(&script.engine.result()), expected, "{stage:?}");
    }
}

#[test]
fn a_late_final_boundary_keeps_the_headline() {
    let mut script = Script::new(plan(Stage::Download, &["a"], false));
    let samples = |at: Duration| vec![down("a", moved(at, STAGE * 2))];
    script.run_until(STAGE - ms(250), samples);
    script.tick(STAGE + ms(1600), ms(1600), &samples(STAGE + ms(1600)), &[]);
    assert!(script.finished());
    let result = script.engine.result();
    assert_eq!(intervals(&result), [(Reason::StageStart, true)]);
    assert!(result.throughput.down.unwrap().rate.is_some());
}

#[test]
fn probers_drain_after_the_window_for_at_most_ten_seconds() {
    let mut script = Script::new(plan(Stage::Latency, &["a"], true));
    let up = [(id("a"), Probe::Up)];
    script.step(Duration::ZERO, &[], &up);
    script.run(|_| Vec::new());
    assert_eq!(script.window(), Some((Duration::ZERO, STAGE)));
    assert_eq!(script.at(&Decision::CloseWindow), Some(STAGE));
    assert_eq!(script.at(&Decision::Finish), Some(STAGE + ms(10_000)));
}

#[test]
fn probes_count_from_the_window_start_including_the_opening_tick() {
    let mut script = Script::new(plan(Stage::Upload, &["a"], true));
    script.step(Duration::ZERO, &[Sample { ready: false, ..sample("a") }], &[]);
    script.step(ms(250), &[up("a", 0, ms(250))], &[(id("a"), Probe::Up)]);
    assert_eq!(script.next, ms(250));
    let reply = |sent| {
        let outcome = ProbeOutcome::Reply { rtt: ms(20), handling: Duration::ZERO };
        (id("a"), Probe::Outcome { sent: script.base + sent, outcome })
    };
    script.step(ms(250), &[up("a", 0, ms(250))], &[reply(ms(100)), reply(ms(300))]);
    assert_eq!(script.window(), Some((ms(250), ms(250) + STAGE)));
    script.run(|at| vec![up("a", moved(at, STAGE * 2), at)]);
    let population = script.engine.result().servers[0].latency.unwrap();
    assert_eq!(population.summary.replies, 1);
}

#[test]
fn latency_failures_are_announced_as_they_happen_and_only_once() {
    let mut script = Script::new(plan(Stage::Download, &["a", "b"], true));
    let samples = |at: Duration| vec![down("a", moved(at, STAGE * 2)), down("b", moved(at, STAGE * 2))];
    let up = [(id("a"), Probe::Up)];
    script.step(Duration::ZERO, &samples(Duration::ZERO), &up);
    script.run_until(STAGE, samples);
    assert_eq!(script.failed(), [(STAGE, "b", Scope::Latency, FailureReason::Timeout)]);
    assert_eq!(script.window().map(|(start, _)| start), Some(STAGE));
    assert!(script.removed().is_empty());

    let mut script = Script::new(plan(Stage::Latency, &["a", "b"], true));
    let ups = [(id("a"), Probe::Up), (id("b"), Probe::Up)];
    script.step(Duration::ZERO, &[], &ups);
    let down = |reason| Probe::Down {
        at: script.base + ms(1000),
        failure: Failure::new(reason, "down"),
    };
    let downs = [(id("b"), down(FailureReason::ServerBusy)), (id("b"), down(FailureReason::ConnectionLost))];
    script.step(ms(1000), &[], &downs);
    assert_eq!(script.failed(), [(ms(1000), "b", Scope::Latency, FailureReason::ServerBusy)]);
    assert!(script.removed().is_empty());
}

#[test]
fn a_member_whose_participant_cannot_open_leaves() {
    let mut script = Script::new(plan(Stage::Download, &["a", "b"], false));
    script
        .departed
        .push((id("b"), Failure::new(FailureReason::PreparationFailed, "no lanes")));
    script.run(|at| vec![down("a", moved(at, STAGE))]);
    assert_eq!(script.removed(), [(Duration::ZERO, "b", FailureReason::PreparationFailed)]);
    let result = script.engine.result();
    assert_eq!(result.failures[0].scope, Scope::Server);
    assert!(result.servers[1].left && result.throughput.down.unwrap().rate.is_some());
}

#[test]
fn a_stop_mid_window_keeps_what_was_measured() {
    let mut script = Script::new(plan(Stage::Download, &["a"], true));
    let up = [(id("a"), Probe::Up)];
    script.step(Duration::ZERO, &[down("a", 0)], &up);
    script.run_until(ms(4000), |at| vec![down("a", moved(at, STAGE))]);
    script.engine.stop(script.base + ms(4100));
    let result = script.engine.result();
    assert_eq!((result.measured, result.stopped), (ms(4100), true));
    assert!(result.failures.is_empty() && result.throughput.down.unwrap().rate.is_some());
    assert!(!result.servers[0].latency.unwrap().complete);
    let decisions = script.log.len();
    script.step(ms(4250), &[down("a", 4_250_000)], &[]);
    assert_eq!(script.log.len(), decisions);
}

/// A latency stage where each server sends its replies, then a download stage where its bytes stop at its time.
fn run(servers: &[(&str, usize, Duration)]) -> Vec<StageResult> {
    let names: Vec<_> = servers.iter().map(|(name, ..)| *name).collect();
    let mut latency = Script::new(plan(Stage::Latency, &names, true));
    let base = latency.base;
    let mut probes: Vec<_> = names.iter().map(|name| (id(name), Probe::Up)).collect();
    for (name, replies, _) in servers {
        for reply in 0..*replies {
            let outcome = ProbeOutcome::Reply { rtt: ms(10), handling: Duration::ZERO };
            probes.push((id(name), Probe::Outcome { sent: base + ms(100 * reply as u64 + 1), outcome }));
        }
    }
    latency.step(Duration::ZERO, &[], &probes[..names.len()]);
    latency.step(ms(5000), &[], &probes[names.len()..]);
    let drained: Vec<_> = names.iter().map(|name| (id(name), Probe::Drained)).collect();
    latency.step(STAGE, &[], &drained);
    let mut download = Script::new(plan(Stage::Download, &names, false));
    download.run(|at| {
        servers
            .iter()
            .map(|&(name, _, stop)| down(name, moved(at, stop)))
            .collect()
    });
    vec![latency.engine.result(), download.engine.result()]
}

#[test]
fn outcomes_follow_stage_results() {
    let plan = [Stage::Latency, Stage::Download];
    let outcome = |servers: &[(&str, usize, Duration)]| Outcome::of(&run(servers), &plan);
    assert_eq!(outcome(&[("a", 5, STAGE), ("b", 5, STAGE)]), Outcome::Complete);
    assert_eq!(outcome(&[("a", 5, STAGE), ("b", 5, ms(3000))]), Outcome::Partial);
    assert_eq!(outcome(&[("a", 5, STAGE), ("b", 0, STAGE)]), Outcome::Partial);
    assert_eq!(outcome(&[("a", 0, STAGE), ("b", 5, STAGE)]), Outcome::Incomplete);

    let mut results = run(&[("a", 5, STAGE)]);
    let unprepared = ServerFailure {
        server: id("b"),
        scope: Scope::Server,
        failure: Failure::new(FailureReason::PreparationFailed, "no path"),
        at: Instant::now(),
    };
    assert_eq!(Outcome::of(&results, &plan), Outcome::Complete);
    results[0].failures.push(unprepared);
    assert_eq!(Outcome::of(&results, &plan), Outcome::Partial);
    assert_eq!(
        Outcome::of(&results, &[Stage::Latency, Stage::Download, Stage::Upload]),
        Outcome::Incomplete
    );

    let mut script = Script::new(self::plan(Stage::Download, &["a"], false));
    script.run(|_| vec![Sample { ready: false, ..down("a", 0) }]);
    let unopened = script.engine.result();
    assert_eq!((unopened.throughput.down, unopened.status(None)), (None, StageStatus::Failed));
    assert_eq!(Outcome::of(&[unopened], &[Stage::Download]), Outcome::Failed);
    let mut stopped = Engine::new(self::plan(Stage::Download, &["a"], false), Instant::now());
    stopped.stop(Instant::now());
    let stopped = stopped.result();
    assert_eq!((stopped.throughput.down, stopped.status(None)), (None, StageStatus::Stopped));
    assert_eq!(Outcome::of(&[stopped], &[Stage::Download]), Outcome::Stopped);
    // Once the first server left, the latency focus is a survivor that measured latency, if any.
    let results = run(&[("a", 5, ms(3000)), ("b", 0, STAGE)]);
    assert_eq!((focus(&results), Outcome::of(&results, &plan)), (None, Outcome::Incomplete));
}
