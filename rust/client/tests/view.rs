//! The view a run's events reduce to: its latency server across a departure, its issues and its traces.
use graphite_meter_client::{
    events::{Event, Series, View},
    measure::latency::{Population, Summary},
    model::{Dir, Failure, Outcome, Scope, ServerFailure, ServerResult, Stage, StageResult},
    run::engine::StagePlan,
};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::time::{Duration, Instant};

const SECOND: Duration = Duration::from_secs(1);

fn id(text: &str) -> ServerId {
    ServerId::parse(text).unwrap()
}

/// A server's share of `stage`, with a latency median when `median` is given.
fn server(name: &str, left: bool, median: Option<Duration>) -> ServerResult {
    let summary = Summary { replies: 5, p50: median, ..Summary::default() };
    let latency = Some(Population { summary, complete: median.is_some() });
    ServerResult { server: id(name), left, throughput: Dir::default(), latency }
}

fn result(stage: Stage, servers: Vec<ServerResult>, failures: Vec<ServerFailure>) -> StageResult {
    let (measured, throughput, intervals) = (SECOND, Dir::default(), Vec::new());
    StageResult {
        stage,
        measured,
        stopped: false,
        throughput,
        servers,
        failures,
        intervals,
        omitted: 0,
    }
}

fn stage(stage: Stage) -> Event {
    Event::StageStarted(StagePlan { stage, members: Vec::new(), duration: SECOND, latency: None })
}

/// A view of a latency then download run over `a` and `b`, where `a` leaves the download after 1.5 s; `c` had no path.
/// The failures its result records, `a`'s announced a second earlier.
fn departed(b_measured: bool) -> (View, [ServerFailure; 2]) {
    let (at, mut view) = (Instant::now(), View::default());
    let plan = vec![(Stage::Latency, SECOND), (Stage::Download, SECOND)];
    let b_median = b_measured.then_some(Duration::from_millis(9));
    let left = ServerFailure {
        server: id("a"),
        scope: Scope::Throughput,
        failure: Failure::new(FailureReason::Timeout, "download bytes stopped growing for 2s"),
        at: at + SECOND * 3,
    };
    let unprepared = ServerFailure {
        server: id("c"),
        scope: Scope::Server,
        failure: Failure::new(FailureReason::PreparationFailed, "no path"),
        at,
    };
    let events = [
        Event::RunStarted { plan, focus: id("a"), at },
        Event::ServerFailed(unprepared.clone()),
        stage(Stage::Latency),
        Event::StageFinished(result(
            Stage::Latency,
            vec![server("a", false, Some(Duration::from_millis(4))), server("b", false, b_median)],
            vec![unprepared.clone()],
        )),
        stage(Stage::Download),
        Event::ServerFailed(ServerFailure { at: at + SECOND * 2, ..left.clone() }),
        Event::StageFinished(result(
            Stage::Download,
            vec![server("a", true, None), server("b", false, None)],
            vec![left.clone()],
        )),
        Event::RunFinished { outcome: Outcome::Partial, error: None, elapsed: SECOND * 4 },
    ];
    events.iter().for_each(|event| view.apply(event));
    (view, [unprepared, left])
}

#[test]
fn the_latency_server_moves_to_a_survivor_that_measured_latency_and_stays_otherwise() {
    let (view, _) = departed(true);
    let run = view.run.unwrap();
    assert_eq!(run.focus, Some(id("b")));
    assert_eq!(run.outcome, Some(Outcome::Partial));
    let (view, _) = departed(false);
    assert_eq!(view.run.unwrap().focus, Some(id("a")), "no survivor measured latency");
}

#[test]
fn a_finished_stage_records_its_failures_as_its_result_does() {
    let (view, [unprepared, left]) = departed(true);
    let issues = view.run.unwrap().issues;
    assert_eq!(issues, [(Stage::Latency, unprepared), (Stage::Download, left)]);
}

#[test]
fn traces_keep_the_planned_span_and_their_peaks_as_they_coarsen() {
    let (at, mut view) = (Instant::now(), View::default());
    let plan = vec![(Stage::Latency, SECOND * 4), (Stage::Download, SECOND * 60)];
    view.apply(&Event::RunStarted { plan, focus: id("a"), at });
    view.apply(&stage(Stage::Download));
    view.apply(&Event::Measuring(Stage::Download));
    for tick in 0..2000 {
        let rate = if tick == 1234 { 9e9 } else { 1e8 };
        let rates = Dir { down: Some(rate), up: None };
        view.apply(&Event::Sample {
            at: Duration::from_millis(tick * 25),
            rates,
            recovering: false,
        });
    }
    let run = view.run.unwrap();
    let down: &Series = &run.throughput.down;
    assert_eq!(down.span(), SECOND * 64);
    let points = down.points();
    assert!(points.len() <= 480, "{}", points.len());
    assert_eq!(
        (points[0].at, points[0].value),
        (SECOND * 4, None),
        "the window breaks the trace after the latency span"
    );
    assert!(points.last().unwrap().at < SECOND * 64);
    let peak = points.iter().map(|point| point.peak).fold(0.0, f64::max);
    assert_eq!(peak, 9e9);
    let means = points.iter().filter_map(|point| point.value);
    assert!(means.clone().all(|mean| mean < 9e9) && means.count() > 100);
    assert!(run.throughput.up.points().iter().all(|point| point.value.is_none()));
}
