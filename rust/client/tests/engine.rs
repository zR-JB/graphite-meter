//! The stage engine against scripted timelines.

use graphite_meter_client::{
    measure::{
        aggregate::{Reason, Receiver},
        latency::ProbeOutcome,
    },
    model::{Cadence, Dir, Failure, LaneHealth, Outcome, Scope, Stage, StageResult},
    run::engine::{Decision, Engine, Input, Probe, Sample, StagePlan, stagger, warmup},
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

/// A checkpoint whose receiver clock runs with the client's.
fn up(server: &str, bytes: u64, at: Duration) -> Sample {
    let counters = Counters::new(bytes, at.as_nanos() as u64 + 1);
    Sample { up: Some(Ok(Receiver { id: 0, counters })), ..sample(server) }
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
        }
    }

    fn tick(&mut self, at: Duration, lateness: Duration, samples: &[Sample], probes: &[(ServerId, Probe)]) {
        let tick = self
            .engine
            .tick(Input { now: self.base + at, lateness, samples, probes });
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

    fn removed(&self) -> Vec<(Duration, &str, FailureReason)> {
        let removals = self.log.iter().filter_map(|(at, decision)| match decision {
            Decision::Remove(server, failure) => Some((*at, server.as_str(), failure.reason)),
            _ => None,
        });
        removals.collect()
    }
}

#[test]
fn a_quiet_link_never_ends_a_stage() {
    let mut script = Script::new(plan(Stage::Download, &["a"], false));
    script.run(|at| {
        let bytes = if at < ms(6000) { moved(at, ms(1000)) } else { moved(at, STAGE) - 5_000_000 };
        vec![down("a", bytes)]
    });
    assert_eq!(script.at(&Decision::OpenWindow), Some(Duration::ZERO));
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
    assert_eq!(script.at(&Decision::OpenWindow), Some(Duration::ZERO));
    assert_eq!(script.removed(), [(ms(3000), "b", FailureReason::SignInRequired)]);
}

#[test]
fn a_late_tick_resumes_evidence_and_a_slow_checkpoint_does_not() {
    for (stage, lateness, resumed) in [(Stage::Download, ms(1600), true), (Stage::Upload, ms(0), false)] {
        let mut script = Script::new(plan(stage, &["a"], false));
        let samples = |at: Duration| {
            vec![Sample {
                down: Some(moved(at, STAGE)),
                ..up("a", moved(at, STAGE), at)
            }]
        };
        script.run_until(ms(3000), samples);
        script.tick(ms(4600), lateness, &samples(ms(4600)), &[]);
        script.run(samples);
        let result = script.engine.result();
        let reasons: Vec<_> = result
            .intervals
            .iter()
            .map(|interval| (interval.reason, interval.complete))
            .collect();
        match resumed {
            true => assert_eq!(reasons, [(Reason::StageStart, false), (Reason::EvidenceResumed, true)], "{stage:?}"),
            false => assert_eq!(reasons, [(Reason::StageStart, true)], "{stage:?}"),
        }
        assert!(script.removed().is_empty());
    }
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
    assert_eq!(script.at(&Decision::OpenWindow), Some(STAGE));
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
    let result = script.engine.result();
    let scopes: Vec<_> = result
        .failures
        .iter()
        .map(|failure| (failure.server.as_str(), failure.scope))
        .collect();
    assert_eq!(scopes, [("b", Scope::Latency)]);
    assert!(result.servers[1].throughput.down.unwrap().rate.is_some());
}

/// A latency stage where each of `servers` sends its replies, then a download stage.
fn run(servers: &[(&str, usize)], download_stop_b: Duration) -> Vec<StageResult> {
    let names: Vec<_> = servers.iter().map(|(name, _)| *name).collect();
    let mut latency = Script::new(plan(Stage::Latency, &names, true));
    let base = latency.base;
    let mut probes: Vec<_> = names.iter().map(|name| (id(name), Probe::Up { at: base })).collect();
    for (name, replies) in servers {
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
        names
            .iter()
            .map(|&name| down(name, moved(at, if name == "b" { download_stop_b } else { STAGE })))
            .collect()
    });
    vec![latency.engine.result(), download.engine.result()]
}

#[test]
fn outcomes_follow_stage_results() {
    let plan = [Stage::Latency, Stage::Download];
    assert_eq!(Outcome::of(&run(&[("a", 5), ("b", 5)], STAGE), &plan, false), Outcome::Complete);
    assert_eq!(Outcome::of(&run(&[("a", 5), ("b", 5)], ms(3000)), &plan, false), Outcome::Partial);
    assert_eq!(Outcome::of(&run(&[("a", 5), ("b", 0)], STAGE), &plan, false), Outcome::Partial);
    assert_eq!(Outcome::of(&run(&[("a", 0), ("b", 5)], STAGE), &plan, false), Outcome::Incomplete);
    let results = run(&[("a", 5)], STAGE);
    assert_eq!(
        Outcome::of(&results, &[Stage::Latency, Stage::Download, Stage::Upload], false),
        Outcome::Incomplete
    );
    assert_eq!(Outcome::of(&results, &plan, true), Outcome::Stopped);

    let mut stopped = Engine::new(self::plan(Stage::Download, &["a"], false), Instant::now());
    stopped.stop(Instant::now());
    let result = stopped.result();
    assert_eq!((result.measured, result.stopped), (Duration::ZERO, true));
    assert_eq!(Outcome::of(&[result], &[Stage::Download], false), Outcome::Failed);
}

#[test]
fn a_stage_without_evidence_records_insufficient_evidence() {
    let results = run(&[("a", 0)], STAGE);
    let reasons: Vec<_> = results[0]
        .failures
        .iter()
        .map(|failure| (failure.scope, failure.failure.reason))
        .collect();
    assert_eq!(reasons, [(Scope::Latency, FailureReason::InsufficientEvidence)]);
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
