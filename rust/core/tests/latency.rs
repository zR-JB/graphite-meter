use graphite_meter_core::latency::{LatencyAccumulator, ProbeOutcome, ReflectorTiming};

const MS: i64 = 1_000_000;

fn reply(stats: &mut LatencyAccumulator, rtt_ms: i64, handling_nanos: u64) -> Option<u64> {
    stats.record(ProbeOutcome::Reply {
        rtt_nanos: rtt_ms * MS,
        handling_nanos,
    })
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
        })
    );
    snapshot.reflector_timing = None;
    assert_eq!(snapshot, raw.snapshot());
    let captured = timed.snapshot();
    reply(&mut timed, 100, 20 * MS as u64);
    assert_eq!(captured.reflector_timing.unwrap().count, 2);
    let mut rounded = LatencyAccumulator::default();
    for _ in 0..10_000 {
        for rtt_nanos in [0, 499, 500, 1_001, 1_499, 1_500] {
            rounded.record(ProbeOutcome::Reply {
                rtt_nanos,
                handling_nanos: 0,
            });
        }
    }
    let summary = rounded.snapshot();
    assert_eq!(summary.count, 60_000);
    assert_eq!(
        summary.distribution.unwrap(),
        graphite_meter_core::latency::Distribution { p50: 1_000, p95: 2_000 }
    );
    assert_eq!(summary.jitter, Some(499));
}

#[test]
fn reflector_duration_bounds_and_large_sums_are_exact() {
    let mut stats = LatencyAccumulator::default();
    for (handling_nanos, expected) in [
        (i64::MAX as u64, Some(i64::MAX as u64)),
        (i64::MAX as u64 + 1, None),
        (u64::MAX, None),
    ] {
        assert_eq!(
            stats.record(ProbeOutcome::Reply {
                rtt_nanos: i64::MAX,
                handling_nanos,
            }),
            expected,
            "handling {handling_nanos}"
        );
    }
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.distribution.unwrap().p50, i64::MAX as u64);
    assert_eq!(snapshot.reflector_timing.unwrap().mean_handling, i64::MAX as u64);
    assert_eq!(snapshot.jitter, Some(0));

    for (rtt_nanos, expected) in [(0, Some(0)), (-1, None)] {
        assert_eq!(
            stats.record(ProbeOutcome::Reply { rtt_nanos, handling_nanos: 0 }),
            expected,
            "RTT {rtt_nanos}"
        );
    }
    assert_eq!(stats.snapshot().count, 4);
}
