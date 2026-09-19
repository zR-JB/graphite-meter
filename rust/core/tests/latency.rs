use graphite_meter_core::latency::{LatencyAccumulator, ProbeOutcome, ReflectorTiming};

const MS: i64 = 1_000_000;

fn reply(stats: &mut LatencyAccumulator, rtt_ms: i64, handling_nanos: u64) -> Option<u64> {
    stats.record(ProbeOutcome::Reply {
        rtt_nanos: rtt_ms * MS,
        handling_nanos,
    })
}

#[test]
fn receive_order_jitter_and_sorted_percentiles_match_go_fixtures() {
    let mut stats = LatencyAccumulator::default();
    for ms in [30, 10, 40, 20] {
        reply(&mut stats, ms, 0);
    }
    stats.record(ProbeOutcome::Timeout);
    stats.record(ProbeOutcome::Timeout);
    let snapshot = stats.snapshot();
    let distribution = snapshot.distribution.unwrap();
    assert_eq!(
        (distribution.min, distribution.max, distribution.mean),
        (10 * MS as u64, 40 * MS as u64, 25 * MS as u64)
    );
    assert_eq!(
        (
            distribution.p10,
            distribution.p50,
            distribution.p90,
            distribution.p95
        ),
        (
            10 * MS as u64,
            25 * MS as u64,
            40 * MS as u64,
            40 * MS as u64
        )
    );
    assert_eq!(snapshot.jitter, Some(70 * MS as u64 / 3));
    assert_eq!(snapshot.jitter_pairs, 3);
    assert_eq!(snapshot.timeout_ratio(), Some(2.0 / 6.0));
    assert!(snapshot.has_observations());

    let mut alternating = LatencyAccumulator::default();
    for ms in [10, 100, 10, 100] {
        reply(&mut alternating, ms, 0);
    }
    let distribution = alternating.snapshot().distribution.unwrap();
    assert_eq!(
        (
            distribution.p10,
            distribution.p50,
            distribution.p90,
            distribution.p95
        ),
        (
            10 * MS as u64,
            55 * MS as u64,
            100 * MS as u64,
            100 * MS as u64
        )
    );
    assert_eq!(alternating.snapshot().jitter, Some(90 * MS as u64));
    reply(&mut alternating, 10, 0);
    assert_eq!(alternating.snapshot().jitter, Some(90 * MS as u64));
}

#[test]
fn missing_outcomes_and_reconnect_keep_jitter_availability_honest() {
    let mut stats = LatencyAccumulator::default();
    let empty = stats.snapshot();
    assert_eq!(empty.timeout_ratio(), None);
    assert_eq!(empty.distribution, None);
    assert_eq!(empty.jitter, None);
    assert!(!empty.has_observations());

    stats.record(ProbeOutcome::Unresolved);
    stats.record(ProbeOutcome::SendFailure);
    assert_eq!(stats.snapshot().timeout_ratio(), None);
    assert!(stats.snapshot().has_observations());
    stats.record(ProbeOutcome::Timeout);
    assert_eq!(stats.snapshot().timeout_ratio(), Some(1.0));
    reply(&mut stats, 10, 0);
    stats.record(ProbeOutcome::Timeout); // Loss does not itself break reply continuity.
    reply(&mut stats, 20, 0);
    stats.break_continuity(); // The coordinator calls this when it reconnects.
    reply(&mut stats, 100, 0);
    reply(&mut stats, 110, 0);
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.jitter_pairs, 2);
    assert_eq!(snapshot.jitter, Some(10 * MS as u64));

    let mut steady = LatencyAccumulator::default();
    reply(&mut steady, 1, 0);
    assert_eq!(steady.snapshot().jitter, None);
    reply(&mut steady, 1, 0);
    assert_eq!(steady.snapshot().jitter, Some(0));
}

#[test]
fn reflector_diagnostic_never_changes_raw_reply_population() {
    let mut raw = LatencyAccumulator::default();
    let mut timed = LatencyAccumulator::default();
    for (rtt, handling) in [
        (10, 2 * MS as u64),
        (20, 0),
        (30, u64::MAX),
        (40, 41 * MS as u64),
        (50, u64::MAX),
    ] {
        reply(&mut raw, rtt, u64::MAX);
        reply(&mut timed, rtt, handling);
    }
    timed.record(ProbeOutcome::Timeout);
    raw.record(ProbeOutcome::Timeout);
    let mut snapshot = timed.snapshot();
    assert_eq!(
        snapshot.reflector_timing,
        Some(ReflectorTiming {
            count: 2,
            mean_raw_rtt: 15 * MS as u64,
            mean_handling: MS as u64,
            mean_adjusted_rtt: 14 * MS as u64,
        })
    );
    snapshot.reflector_timing = None;
    assert_eq!(snapshot, raw.snapshot());
    let captured = timed.snapshot();
    reply(&mut timed, 100, 20 * MS as u64);
    assert_eq!(captured.reflector_timing.unwrap().count, 2);
}

#[test]
fn reflector_duration_bounds_and_large_sums_are_exact() {
    let mut stats = LatencyAccumulator::default();
    assert_eq!(
        stats.record(ProbeOutcome::Reply {
            rtt_nanos: i64::MAX,
            handling_nanos: i64::MAX as u64
        }),
        Some(i64::MAX as u64)
    );
    assert_eq!(
        stats.record(ProbeOutcome::Reply {
            rtt_nanos: i64::MAX,
            handling_nanos: i64::MAX as u64 + 1
        }),
        None
    );
    assert_eq!(
        stats.record(ProbeOutcome::Reply {
            rtt_nanos: i64::MAX,
            handling_nanos: u64::MAX
        }),
        None
    );
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.distribution.unwrap().mean, i64::MAX as u64);
    assert_eq!(
        snapshot.reflector_timing.unwrap().mean_handling,
        i64::MAX as u64
    );
    assert_eq!(snapshot.jitter, Some(0));

    assert_eq!(
        stats.record(ProbeOutcome::Reply {
            rtt_nanos: 0,
            handling_nanos: 0
        }),
        None
    );
    assert_eq!(
        stats.record(ProbeOutcome::Reply {
            rtt_nanos: -1,
            handling_nanos: 0
        }),
        None
    );
    assert_eq!(stats.snapshot().count, 3);
}
