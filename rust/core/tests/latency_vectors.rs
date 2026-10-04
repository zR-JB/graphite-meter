use graphite_meter_core::latency::{DeadlineEstimator, LatencyAccumulator, ProbeOutcome};
use serde_json::Value;

#[test]
fn latency_vectors() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("../../../api/latency.testvectors.json")).unwrap();
    for case in &cases {
        let mut accumulator = LatencyAccumulator::default();
        let mut estimator = DeadlineEstimator::default();
        let mut deadlines = vec![estimator.deadline_nanos() as f64 / 1e6];
        for outcome in case["outcomes"].as_array().unwrap() {
            if outcome["break"] == true {
                accumulator.break_continuity();
            } else if outcome["timeout"] == true {
                accumulator.record(ProbeOutcome::Timeout);
            } else {
                let rtt_nanos = (outcome["rttMs"].as_f64().unwrap() * 1e6) as i64;
                accumulator.record(ProbeOutcome::Reply { rtt_nanos, handling_nanos: 0 });
            }
            if let Some(rtt) = outcome["rttMs"].as_f64() {
                estimator.observe((rtt * 1e6) as u64);
                deadlines.push(estimator.deadline_nanos() as f64 / 1e6);
            }
        }
        if let Some(expected) = case["deadlineMs"].as_array() {
            let expected: Vec<_> = expected.iter().map(|deadline| deadline.as_f64().unwrap()).collect();
            assert_eq!(deadlines, expected, "{}", case["name"]);
        }
        let summary = accumulator.snapshot();
        let ms = |nanos: Option<u64>| nanos.map(|nanos| nanos as f64 / 1e6);
        for (field, actual) in [
            ("replies", Some(summary.count as f64)),
            ("timeouts", Some(summary.timeouts as f64)),
            ("timeoutRatio", summary.timeout_ratio()),
            ("p50Ms", ms(summary.distribution.map(|distribution| distribution.p50))),
            ("p95Ms", ms(summary.distribution.map(|distribution| distribution.p95))),
            ("jitterMs", ms(summary.jitter)),
            ("jitterPairs", Some(summary.jitter_pairs as f64)),
        ] {
            let expected = case["expect"][field].as_f64();
            let equal = actual
                .zip(expected)
                .map_or(actual == expected, |(a, e)| (a - e).abs() < 1e-6);
            assert!(equal, "{} {field}: {actual:?} != {expected:?}", case["name"]);
        }
    }
}
