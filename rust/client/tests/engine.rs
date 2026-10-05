//! The stage engine against scripted timelines.

use graphite_meter_client::{
    measure::{
        aggregate::{Fed, Reason, Receiver},
        latency::ProbeOutcome,
    },
    model::{Cadence, Dir, Failure, LaneHealth, Outcome, Scope, ServerFailure, Stage, StageResult, focus},
    run::engine::{Decision, Engine, Input, Probe, Sample, StagePlan, lateness, stagger, warmup},
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
        members: members.iter().map(|member| id(member)).collect(),
        warmup: Duration::ZERO,
        duration: STAGE,
        latency: latency.then_some(Cadence::ReplyDriven),
    }
}

/// Bytes of a server moving one kilobyte per millisecond until `stop`.
fn moved(at: Duration, stop: Duration) -> u64 {
    at.min(stop).as_millis() as u64 * 1000
}

fn sample(server: &str) -> Sample {
    let lanes = Dir { down: LaneHealth::Ok, up: LaneHealth::Ok };
    Sample {
        server: id(server),
        ready: true,
        down: None,
        up: None,
        fed: None,
        lanes,
    }
}

fn down(server: &str, bytes: u64) -> Sample {
    Sample { down: Some(bytes), ..sample(server) }
}

fn checkpoint(server: &str, bytes: u64, nanos: u64) -> Sample {
    let counters = Counters::new(bytes, nanos);
    Sample { up: Some(Ok(Receiver { id: 0, counters })), ..sample(server) }
}

/// A checkpoint whose receiver clock runs with the client's.
fn up(server: &str, bytes: u64, at: Duration) -> Sample {
    checkpoint(server, bytes, at.as_nanos() as u64 + 1)
}

fn missed(server: &str, reason: FailureReason) -> Sample {
    Sample {
        up: Some(Err(Failure::new(reason, "checkpoint missed"))),
        ..sample(server)
    }
}

/// An engine ticked at scripted times, with what it decided.
struct Script {
    engine: Engine,
    base: Instant,
    next: Duration,
    log: Vec<(Duration, Decision)>,
    recovering: Vec<Duration>,
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
            departed: Vec::new(),
        }
    }

    fn tick(&mut self, at: Duration, lateness: Duration, samples: &[Sample], probes: &[(ServerId, Probe)]) {
        let departed = std::mem::take(&mut self.departed);
        let tick = self.engine.tick(Input {
            now: self.base + at,
            lateness,
            samples,
            probes,
            departed: &departed,
        });
        self.next = tick.next - self.base;
        self.log
            .extend(tick.decisions.into_iter().map(|decision| (at, decision)));
        if tick.recovering {
            self.recovering.push(at);
        }
    }

    /// Ticks when the engine asks, through `until` or its finish.
    fn run_until(&mut self, until: Duration, samples: impl Fn(Duration) -> Vec<Sample>) {
        while !self.finished() && self.next <= until {
            let at = self.next;
            self.tick(at, Duration::ZERO, &samples(at), &[]);
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
        self.log
            .iter()
            .find(|(_, decision)| decision == wanted)
            .map(|(at, _)| *at)
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
fn a_quiet_link_never_ends_a_stage() {
    let mut script = Script::new(plan(Stage::Download, &["a"], false));
    script.run(|at| {
        let bytes = if at < ms(6000) { moved(at, ms(1000)) } else { moved(at, STAGE) - 5_000_000 };
        vec![down("a", bytes)]
    });
    assert_eq!(script.window(), Some((Duration::ZERO, STAGE)));
    assert_eq!(script.at(&Decision::CloseWindow), Some(STAGE));
    assert_eq!(script.at(&Decision::Finish), Some(STAGE));
    assert!(script.removed().is_empty());
    assert_eq!(script.recovering.first(), Some(&ms(3000)));
    assert!(script.recovering.iter().all(|&at| at <= ms(6000)));
    let result = script.engine.result();
    assert_eq!(result.measured, STAGE);
    assert!(result.throughput.down.unwrap().rate.is_some());
}

#[test]
fn a_server_silent_while_another_grows_leaves_where_it_last_moved() {
    let mut script = Script::new(plan(Stage::Download, &["a", "b"], false));
    script.run(|at| vec![down("a", moved(at, STAGE)), down("b", moved(at, ms(3000)))]);
    assert_eq!(script.removed(), [(ms(5000), "b", FailureReason::Timeout)]);
    assert!(script.recovering.is_empty());
    let result = script.engine.result();
    let ends: Vec<_> = result
        .intervals
        .iter()
        .map(|interval| (interval.reason, interval.start - script.base))
        .collect();
    assert_eq!(ends, [(Reason::StageStart, Duration::ZERO), (Reason::Dropout, ms(3000))]);
    assert_eq!(result.intervals[0].end - script.base, ms(3000));
    assert!(result.servers[1].left);
    assert_eq!(result.failures[0].scope, Scope::Throughput);
}

#[test]
fn shared_silence_removes_nobody_and_shows_recovering() {
    let mut script = Script::new(plan(Stage::Download, &["a", "b"], false));
    let bytes = |at: Duration| if at < ms(5000) { moved(at, ms(2000)) } else { moved(at, STAGE) - 3_000_000 };
    script.run(|at| vec![down("a", bytes(at)), down("b", bytes(at))]);
    assert!(script.removed().is_empty());
    assert_eq!(script.recovering, [ms(4000), ms(4250), ms(4500), ms(4750), ms(5000)]);
    assert_eq!(script.at(&Decision::Finish), Some(STAGE));
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
        vec![up("a", moved(at, STAGE), at), Sample { fed: Some(fed), ..checkpoint("b", 1000, 1) }]
    });
    assert!(script.removed().is_empty());
}

#[test]
fn three_missed_checkpoints_remove_only_while_another_moves_and_not_at_the_end() {
    let timeout = FailureReason::Timeout;
    let mut script = Script::new(plan(Stage::Upload, &["a", "b"], false));
    script.run(|at| {
        let b = if at < ms(2000) { up("b", moved(at, STAGE), at) } else { missed("b", timeout) };
        vec![up("a", moved(at, STAGE), at), b]
    });
    assert_eq!(script.removed(), [(ms(2500), "b", timeout)]);

    let mut script = Script::new(plan(Stage::Upload, &["a", "b"], false));
    script.run(|at| {
        let b = if at < ms(9500) { up("b", moved(at, STAGE), at) } else { missed("b", timeout) };
        vec![up("a", moved(at, STAGE), at), b]
    });
    assert!(script.removed().is_empty());

    let mut script = Script::new(plan(Stage::Upload, &["a", "b"], false));
    script.run_until(ms(9750), |at| match at < ms(2000) {
        true => vec![up("a", moved(at, STAGE), at), up("b", moved(at, STAGE), at)],
        false => vec![missed("a", timeout), missed("b", timeout)],
    });
    assert!(script.removed().is_empty());
}

#[test]
fn a_refused_grant_leaves_at_once_asking_for_sign_in() {
    let mut script = Script::new(plan(Stage::Upload, &["a", "b"], false));
    script.run(|at| {
        let b = if at == ms(3000) {
            missed("b", FailureReason::SignInRequired)
        } else {
            up("b", moved(at, STAGE), at)
        };
        vec![up("a", moved(at, STAGE), at), b]
    });
    assert_eq!(script.window(), Some((Duration::ZERO, STAGE)));
    assert_eq!(script.removed(), [(ms(3000), "b", FailureReason::SignInRequired)]);
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
        let samples = |at: Duration| {
            vec![Sample {
                down: Some(moved(at, STAGE)),
                ..up("a", moved(at, STAGE), at)
            }]
        };
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
    let up = [(id("a"), Probe::Up { at: script.base })];
    script.tick(Duration::ZERO, Duration::ZERO, &[], &up);
    script.run(|_| Vec::new());
    assert_eq!(script.window(), Some((Duration::ZERO, STAGE)));
    assert_eq!(script.at(&Decision::CloseWindow), Some(STAGE));
    assert_eq!(script.at(&Decision::Finish), Some(STAGE + ms(10_000)));
}

#[test]
fn the_final_boundary_removes_silent_and_quietly_retrying_servers() {
    let mut script = Script::new(plan(Stage::Download, &["a"], false));
    script.run(|at| vec![down("a", moved(at, ms(7000)))]);
    assert_eq!(script.removed(), [(STAGE, "a", FailureReason::Timeout)]);

    let retrying = || Dir {
        down: LaneHealth::Retrying(Failure::new(FailureReason::ServerBusy, "busy")),
        up: LaneHealth::Ok,
    };
    let mut script = Script::new(plan(Stage::Download, &["a", "b"], false));
    script.run(|at| {
        let (mut a, mut b) = (down("a", moved(at, ms(9500))), down("b", moved(at, STAGE)));
        if at >= ms(9000) {
            (a.lanes, b.lanes) = (retrying(), retrying());
        }
        vec![a, b]
    });
    assert_eq!(script.removed(), [(STAGE, "a", FailureReason::ServerBusy)]);
    let result = script.engine.result();
    assert_eq!(result.servers.iter().map(|server| server.left).collect::<Vec<_>>(), [true, false]);
}

#[test]
fn readiness_past_ten_seconds_removes_the_unready() {
    let mut script = Script::new(plan(Stage::Download, &["a", "b"], false));
    script.run(|at| vec![down("a", moved(at, STAGE * 2)), Sample { ready: false, ..down("b", 0) }]);
    assert_eq!(script.removed(), [(STAGE, "b", FailureReason::Timeout)]);
    assert_eq!(script.window(), Some((STAGE, STAGE * 2)));
    assert_eq!(script.at(&Decision::Finish), Some(STAGE * 2));
}

#[test]
fn a_lost_latency_channel_leaves_only_in_the_latency_stage() {
    let lost = |at| Probe::Down {
        at,
        failure: Failure::new(FailureReason::ConnectionLost, "reset"),
    };
    let reply = |base: Instant, sent| Probe::Outcome {
        sent: base + sent,
        outcome: ProbeOutcome::Reply { rtt: ms(20), handling: Duration::ZERO },
    };
    let mut script = Script::new(plan(Stage::Latency, &["a", "b"], true));
    let (a, b, base) = (id("a"), id("b"), script.base);
    script.tick(
        Duration::ZERO,
        Duration::ZERO,
        &[],
        &[(a.clone(), Probe::Up { at: base }), (b.clone(), Probe::Up { at: base })],
    );
    script.tick(
        ms(1000),
        Duration::ZERO,
        &[],
        &[(a.clone(), reply(base, ms(900))), (b.clone(), lost(base + ms(1000)))],
    );
    assert_eq!(script.removed(), [(ms(1000), "b", FailureReason::ConnectionLost)]);
    script.tick(STAGE, Duration::ZERO, &[], &[(a.clone(), reply(base, ms(9990)))]);
    assert!(!script.finished());
    script.tick(
        STAGE + ms(250),
        Duration::ZERO,
        &[],
        &[(a.clone(), reply(base, STAGE)), (a.clone(), Probe::Drained)],
    );
    assert!(script.finished());
    let population = script.engine.result().servers[0].latency.unwrap();
    assert_eq!((population.summary.replies, population.complete), (2, true));

    let mut script = Script::new(plan(Stage::Download, &["a", "b"], true));
    let ups = [(id("a"), Probe::Up { at: script.base }), (id("b"), Probe::Up { at: script.base })];
    script.tick(Duration::ZERO, Duration::ZERO, &[down("a", 0), down("b", 0)], &ups);
    let probes = [(id("b"), lost(script.base + ms(1000)))];
    script.tick(ms(1000), Duration::ZERO, &[down("a", 1_000_000), down("b", 1_000_000)], &probes);
    let samples = [down("a", 10_000_000), down("b", 10_000_000)];
    script.tick(STAGE - ms(250), Duration::ZERO, &samples, &[]);
    script.tick(STAGE, Duration::ZERO, &samples, &[(id("a"), Probe::Drained)]);
    assert!(script.finished() && script.removed().is_empty());
    assert_eq!(script.failed(), [(ms(1000), "b", Scope::Latency, FailureReason::ConnectionLost)]);
    assert!(
        script.engine.result().servers[1]
            .throughput
            .down
            .unwrap()
            .rate
            .is_some()
    );
}

#[test]
fn latency_failures_are_announced_as_they_happen_and_only_once() {
    let mut script = Script::new(plan(Stage::Download, &["a", "b"], true));
    let samples = |at: Duration| vec![down("a", moved(at, STAGE * 2)), down("b", moved(at, STAGE * 2))];
    let up = [(id("a"), Probe::Up { at: script.base })];
    script.tick(Duration::ZERO, Duration::ZERO, &samples(Duration::ZERO), &up);
    script.run_until(STAGE, samples);
    assert_eq!(script.failed(), [(STAGE, "b", Scope::Latency, FailureReason::Timeout)]);
    assert_eq!(script.window().map(|(start, _)| start), Some(STAGE));
    assert!(script.removed().is_empty());

    let mut script = Script::new(plan(Stage::Latency, &["a", "b"], true));
    let ups = [(id("a"), Probe::Up { at: script.base }), (id("b"), Probe::Up { at: script.base })];
    script.tick(Duration::ZERO, Duration::ZERO, &[], &ups);
    let down = |reason| Probe::Down {
        at: script.base + ms(1000),
        failure: Failure::new(reason, "down"),
    };
    let downs = [(id("b"), down(FailureReason::ServerBusy)), (id("b"), down(FailureReason::ConnectionLost))];
    script.tick(ms(1000), Duration::ZERO, &[], &downs);
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
    let up = [(id("a"), Probe::Up { at: script.base })];
    script.tick(Duration::ZERO, Duration::ZERO, &[down("a", 0)], &up);
    script.run_until(ms(4000), |at| vec![down("a", moved(at, STAGE))]);
    script.engine.stop(script.base + ms(4100));
    let result = script.engine.result();
    assert_eq!((result.measured, result.stopped), (ms(4100), true));
    assert!(result.failures.is_empty() && result.throughput.down.unwrap().rate.is_some());
    assert!(!result.servers[0].latency.unwrap().complete);
    let decisions = script.log.len();
    script.tick(ms(4250), Duration::ZERO, &[down("a", 4_250_000)], &[]);
    assert_eq!(script.log.len(), decisions);
}

/// A latency stage where each server sends its replies, then a download stage where its bytes stop at its time.
fn run(servers: &[(&str, usize, Duration)]) -> Vec<StageResult> {
    let names: Vec<_> = servers.iter().map(|(name, ..)| *name).collect();
    let mut latency = Script::new(plan(Stage::Latency, &names, true));
    let base = latency.base;
    let mut probes: Vec<_> = names.iter().map(|name| (id(name), Probe::Up { at: base })).collect();
    for (name, replies, _) in servers {
        for reply in 0..*replies {
            let outcome = ProbeOutcome::Reply { rtt: ms(10), handling: Duration::ZERO };
            probes.push((id(name), Probe::Outcome { sent: base + ms(100 * reply as u64 + 1), outcome }));
        }
    }
    latency.tick(Duration::ZERO, Duration::ZERO, &[], &probes[..names.len()]);
    latency.tick(ms(5000), Duration::ZERO, &[], &probes[names.len()..]);
    let drained: Vec<_> = names.iter().map(|name| (id(name), Probe::Drained)).collect();
    latency.tick(STAGE, Duration::ZERO, &[], &drained);
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
    let outcome = |servers: &[(&str, usize, Duration)]| Outcome::of(&run(servers), &plan, &[]);
    assert_eq!(outcome(&[("a", 5, STAGE), ("b", 5, STAGE)]), Outcome::Complete);
    assert_eq!(outcome(&[("a", 5, STAGE), ("b", 5, ms(3000))]), Outcome::Partial);
    assert_eq!(outcome(&[("a", 5, STAGE), ("b", 0, STAGE)]), Outcome::Partial);
    assert_eq!(outcome(&[("a", 0, STAGE), ("b", 5, STAGE)]), Outcome::Incomplete);

    let results = run(&[("a", 5, STAGE)]);
    let unprepared = ServerFailure {
        server: id("b"),
        scope: Scope::Server,
        failure: Failure::new(FailureReason::PreparationFailed, "no path"),
        at: Instant::now(),
    };
    assert_eq!(Outcome::of(&results, &plan, &[unprepared]), Outcome::Partial);
    assert_eq!(
        Outcome::of(&results, &[Stage::Latency, Stage::Download, Stage::Upload], &[]),
        Outcome::Incomplete
    );

    let mut script = Script::new(self::plan(Stage::Download, &["a"], false));
    script.run(|_| vec![Sample { ready: false, ..down("a", 0) }]);
    assert_eq!(Outcome::of(&[script.engine.result()], &[Stage::Download], &[]), Outcome::Failed);
    let mut stopped = Engine::new(self::plan(Stage::Download, &["a"], false), Instant::now());
    stopped.stop(Instant::now());
    assert_eq!(Outcome::of(&[stopped.result()], &[Stage::Download], &[]), Outcome::Stopped);
}

#[test]
fn the_latency_focus_moves_to_a_survivor_that_measured_latency() {
    let plan = [Stage::Latency, Stage::Download];
    let results = run(&[("a", 5, ms(3000)), ("b", 5, STAGE)]);
    assert_eq!(focus(&results), Some(id("b")));
    assert_eq!(Outcome::of(&results, &plan, &[]), Outcome::Partial);
    let results = run(&[("a", 5, ms(3000)), ("b", 0, STAGE)]);
    assert_eq!(focus(&results), None);
    assert_eq!(Outcome::of(&results, &plan, &[]), Outcome::Incomplete);
}

#[test]
fn a_stage_without_evidence_records_insufficient_evidence() {
    let results = run(&[("a", 0, STAGE)]);
    let reasons: Vec<_> = results[0]
        .failures
        .iter()
        .map(|failure| (failure.scope, failure.failure.reason))
        .collect();
    assert_eq!(reasons, [(Scope::Latency, FailureReason::InsufficientEvidence)]);
    let mut script = Script::new(plan(Stage::Latency, &["a"], true));
    script.tick(Duration::ZERO, Duration::ZERO, &[], &[(id("a"), Probe::Up { at: script.base })]);
    script.run(|_| Vec::new());
    let insufficient = (STAGE + ms(10_000), "a", Scope::Latency, FailureReason::InsufficientEvidence);
    assert_eq!(script.failed(), [insufficient]);
    let mut script = Script::new(plan(Stage::Download, &["a"], false));
    script.run(|_| vec![down("a", 0)]);
    let reasons: Vec<_> = script
        .engine
        .result()
        .failures
        .iter()
        .map(|failure| failure.failure.reason)
        .collect();
    assert_eq!(reasons, [FailureReason::Timeout]);
}

#[test]
fn warmup_stretches_to_ten_idle_round_trips_within_four_seconds() {
    assert_eq!(warmup(ms(800), &[ms(50), ms(120)]), ms(1200));
    assert_eq!(warmup(ms(800), &[ms(20)]), ms(800));
    assert_eq!(warmup(ms(800), &[ms(900)]), ms(4000));
    assert_eq!(warmup(Duration::ZERO, &[]), Duration::ZERO);
    assert_eq!(stagger(ms(400), 5), ms(50));
    assert_eq!(stagger(ms(4000), 6), ms(75));
    assert_eq!(stagger(ms(4000), 1), Duration::ZERO);
}
