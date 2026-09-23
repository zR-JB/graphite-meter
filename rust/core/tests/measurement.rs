use graphite_meter_core::measurement::{
    AggregateMeasurements, Boundary, Direction, IntervalReason, MAX_INTERVALS, MIN_SURVIVOR_NANOS,
    ObservedUpload, ReceiverSnapshot, Stage, UnavailableReason,
};

const MS: u64 = 1_000_000;

fn boundary(ms: u64, down: &[(&str, u64)], up: &[(&str, &str, u64, u64)]) -> Boundary {
    Boundary {
        at_nanos: ms * MS,
        down: down
            .iter()
            .map(|(id, bytes)| (id.to_string(), *bytes))
            .collect(),
        up: up
            .iter()
            .map(|(server, id, bytes, receiver_ms)| {
                (
                    server.to_string(),
                    ReceiverSnapshot {
                        id: id.to_string(),
                        bytes: *bytes,
                        nanos: receiver_ms * MS,
                        requested_at_nanos: 0,
                        received_at_nanos: 0,
                    },
                )
            })
            .collect(),
        ..Boundary::default()
    }
}

fn start(
    engine: &mut AggregateMeasurements,
    stage: Stage,
    ids: &[&str],
    ms: u64,
    reason: IntervalReason,
) {
    engine.begin(
        stage,
        ids.iter().map(|id| id.to_string()).collect(),
        ms * MS,
        reason,
    );
}

#[test]
fn receiver_rates_keep_each_server_clock_and_unique_byte_ledger() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Upload,
        &["a", "b"],
        0,
        IntervalReason::StageStart,
    );
    assert!(
        engine
            .observe(boundary(
                0,
                &[],
                &[("a", "a", 100, 100), ("b", "b", 200, 100)]
            ))
            .is_none()
    );
    let sample = engine
        .observe(boundary(
            1000,
            &[],
            &[("a", "a", 1100, 1100), ("b", "b", 6200, 2100)],
        ))
        .unwrap();
    assert_eq!(sample.up_bytes_per_sec, Some(4000.0));
    assert_eq!(
        (sample.up[0].duration_nanos, sample.up[1].duration_nanos),
        (1000 * MS, 2000 * MS)
    );
    let result = engine.result(Stage::Upload, Direction::Up);
    assert_eq!(result.mean_bytes_per_sec, Some(4000.0));
    assert_eq!(result.total_bytes, 7000);
    assert_eq!(result.unavailable_reason, None);
    assert_eq!(
        engine.server_rate(Stage::Upload, Direction::Up, "a"),
        Some(1000.0)
    );
    assert_eq!(
        engine.server_rate(Stage::Upload, Direction::Up, "b"),
        Some(3000.0)
    );
    assert_eq!(
        engine.server_rate(Stage::Upload, Direction::Up, "missing"),
        None
    );
}

#[test]
fn opposite_fluctuations_have_one_coordinated_peak_and_dropout_revokes_headline() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Download,
        &["a", "b"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(0, &[("a", 0), ("b", 0)], &[]));
    engine.observe(boundary(1000, &[("a", 1000), ("b", 3000)], &[]));
    engine.observe(boundary(2000, &[("a", 4000), ("b", 4000)], &[]));
    let result = engine.result(Stage::Download, Direction::Down);
    assert_eq!(
        (
            result.mean_bytes_per_sec,
            result.peak_bytes_per_sec,
            result.total_bytes
        ),
        (Some(4000.0), Some(4000.0), 8000)
    );
    start(
        &mut engine,
        Stage::Download,
        &["b"],
        2000,
        IntervalReason::Dropout,
    );
    engine.observe(boundary(2100, &[("b", 4200)], &[]));
    engine.observe(boundary(2500, &[("b", 5000)], &[]));
    let result = engine.result(Stage::Download, Direction::Down);
    assert_eq!(result.mean_bytes_per_sec, None);
    assert_eq!(result.total_bytes, 9000);
    assert_eq!(
        engine.server_rate(Stage::Download, Direction::Down, "a"),
        None
    );
    assert_eq!(
        engine.server_rate(Stage::Download, Direction::Down, "b"),
        None
    );
    assert_eq!(
        engine.intervals()[0]
            .window
            .as_ref()
            .unwrap()
            .down_bytes_per_sec,
        Some(4000.0)
    );
    engine.observe(boundary(3500, &[("b", 6000)], &[]));
    assert!(
        engine
            .server_rate(Stage::Download, Direction::Down, "b")
            .is_some()
    );
    assert_eq!(
        engine.server_rate(Stage::Download, Direction::Down, "a"),
        None
    );
}

#[test]
fn measured_zero_is_distinct_from_missing_and_recovery_keeps_unique_bytes() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Upload,
        &["a"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(0, &[], &[("a", "id", 100, 100)]));
    engine.observe(boundary(1000, &[], &[("a", "id", 100, 1100)]));
    assert_eq!(
        engine
            .result(Stage::Upload, Direction::Up)
            .mean_bytes_per_sec,
        Some(0.0)
    );
    let mut missing = boundary(1200, &[], &[]);
    missing.observed_up.insert(
        "a".into(),
        ObservedUpload {
            id: "id".into(),
            maximum: 500,
        },
    );
    engine.observe(missing);
    let result = engine.result(Stage::Upload, Direction::Up);
    assert_eq!(result.mean_bytes_per_sec, None);
    assert_eq!(result.total_bytes, 400);
    engine.observe(boundary(1500, &[], &[("a", "id", 700, 1600)]));
    engine.observe(boundary(2500, &[], &[("a", "id", 1700, 2600)]));
    let result = engine.result(Stage::Upload, Direction::Up);
    assert_eq!(
        (result.mean_bytes_per_sec, result.total_bytes),
        (Some(1000.0), 1600)
    );
    assert_eq!(
        engine.intervals().back().unwrap().reason,
        IntervalReason::EvidenceResumed
    );
}

#[test]
fn bidirectional_membership_is_common_and_all_failed_has_no_old_rate() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Bidirectional,
        &["a", "b"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(
        0,
        &[("a", 0), ("b", 0)],
        &[("a", "a", 0, 100), ("b", "b", 0, 100)],
    ));
    engine.observe(boundary(
        1000,
        &[("a", 1000), ("b", 2000)],
        &[("a", "a", 1000, 1100), ("b", "b", 6000, 2100)],
    ));
    assert_eq!(
        engine
            .result(Stage::Bidirectional, Direction::Down)
            .mean_bytes_per_sec,
        Some(3000.0)
    );
    assert_eq!(
        engine
            .result(Stage::Bidirectional, Direction::Up)
            .mean_bytes_per_sec,
        Some(4000.0)
    );
    start(
        &mut engine,
        Stage::Bidirectional,
        &[],
        1000,
        IntervalReason::Dropout,
    );
    assert_eq!(
        engine
            .result(Stage::Bidirectional, Direction::Down)
            .mean_bytes_per_sec,
        None
    );
    assert_eq!(
        engine
            .result(Stage::Bidirectional, Direction::Up)
            .mean_bytes_per_sec,
        None
    );
}

#[test]
fn interval_history_is_bounded_and_ids_continue_across_eviction() {
    let mut engine = AggregateMeasurements::default();
    for i in 0..140 {
        start(
            &mut engine,
            Stage::Download,
            &["a"],
            i * 1000,
            IntervalReason::StageStart,
        );
        engine.observe(boundary(i * 1000, &[("a", 0)], &[]));
        engine.observe(boundary((i + 1) * 1000, &[("a", 1000)], &[]));
    }
    assert_eq!(engine.intervals().len(), MAX_INTERVALS);
    assert_eq!(engine.omitted_intervals(), 12);
    assert_eq!(engine.intervals().front().unwrap().id, 12);
    assert_eq!(engine.intervals().back().unwrap().id, 139);
    assert_eq!(
        engine
            .result(Stage::Download, Direction::Down)
            .mean_bytes_per_sec,
        Some(1000.0)
    );
}

#[test]
fn receiver_regression_revokes_rate_but_retains_previous_credit() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Upload,
        &["a"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(0, &[], &[("a", "id", 1000, 1000)]));
    engine.observe(boundary(1000, &[], &[("a", "id", 3000, 3000)]));
    assert_eq!(
        engine
            .result(Stage::Upload, Direction::Up)
            .mean_bytes_per_sec,
        Some(1000.0)
    );
    assert_eq!(
        engine.result(Stage::Upload, Direction::Up).total_bytes,
        2000
    );
    engine.observe(boundary(1500, &[], &[("a", "id", 3000, 1500)]));
    let result = engine.result(Stage::Upload, Direction::Up);
    assert_eq!(
        result.unavailable_reason,
        Some(UnavailableReason::SurvivorEvidence)
    );
    assert_eq!(result.total_bytes, 2000);
    assert!(engine.intervals()[0].window.is_some());
}

#[test]
fn current_interval_and_every_receiver_component_need_eight_hundred_ms() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Upload,
        &["a", "b"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(0, &[], &[("a", "a", 0, 0), ("b", "b", 0, 0)]));
    engine.observe(boundary(
        1000,
        &[],
        &[("a", "a", 1000, 1000), ("b", "b", 700, 700)],
    ));
    let result = engine.result(Stage::Upload, Direction::Up);
    assert_eq!(
        result.unavailable_reason,
        Some(UnavailableReason::ComponentEvidence)
    );
    assert_eq!(result.total_bytes, 1700);
    engine.observe(boundary(
        1200,
        &[],
        &[("a", "a", 1200, 1200), ("b", "b", 900, 900)],
    ));
    assert_eq!(
        engine
            .result(Stage::Upload, Direction::Up)
            .mean_bytes_per_sec,
        Some(2000.0)
    );
    assert_eq!(MIN_SURVIVOR_NANOS, 800 * MS);
}

#[test]
fn receiver_identity_change_credits_new_bytes_without_splicing_clocks() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Upload,
        &["a"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(0, &[], &[("a", "old", 0, 0)]));
    engine.observe(boundary(1000, &[], &[("a", "old", 1000, 1000)]));
    engine.observe(boundary(1500, &[], &[("a", "new", 200, 200)]));
    let result = engine.result(Stage::Upload, Direction::Up);
    assert_eq!(result.mean_bytes_per_sec, None);
    assert_eq!(result.total_bytes, 1200);
    engine.observe(boundary(2500, &[], &[("a", "new", 1200, 1200)]));
    let result = engine.result(Stage::Upload, Direction::Up);
    assert_eq!(
        (result.mean_bytes_per_sec, result.total_bytes),
        (Some(1000.0), 2200)
    );
}

#[test]
fn observed_upload_maximum_is_not_double_credited_by_lagging_snapshot() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Upload,
        &["a"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(0, &[], &[("a", "id", 100, 100)]));
    let mut lagging = boundary(1000, &[], &[("a", "id", 300, 1100)]);
    lagging.observed_up.insert(
        "a".into(),
        ObservedUpload {
            id: "id".into(),
            maximum: 500,
        },
    );
    engine.observe(lagging);
    assert_eq!(engine.result(Stage::Upload, Direction::Up).total_bytes, 400);
    engine.observe(boundary(2000, &[], &[("a", "id", 600, 2100)]));
    assert_eq!(engine.result(Stage::Upload, Direction::Up).total_bytes, 500);
}

#[test]
fn download_regression_never_credits_replayed_bytes_or_keeps_old_rate() {
    let mut engine = AggregateMeasurements::default();
    start(
        &mut engine,
        Stage::Download,
        &["a"],
        0,
        IntervalReason::StageStart,
    );
    engine.observe(boundary(0, &[("a", 100)], &[]));
    engine.observe(boundary(1000, &[("a", 1100)], &[]));
    assert_eq!(
        engine.result(Stage::Download, Direction::Down).total_bytes,
        1000
    );
    engine.observe(boundary(1500, &[("a", 500)], &[]));
    assert_eq!(
        engine
            .result(Stage::Download, Direction::Down)
            .mean_bytes_per_sec,
        None
    );
    assert_eq!(
        engine.result(Stage::Download, Direction::Down).total_bytes,
        1000
    );
    engine.observe(boundary(2000, &[("a", 1000)], &[]));
    engine.observe(boundary(3500, &[("a", 1500)], &[]));
    let result = engine.result(Stage::Download, Direction::Down);
    assert_eq!(result.mean_bytes_per_sec, Some(500.0));
    assert_eq!(result.total_bytes, 1400);
}
