use graphite_meter_core::latency::{DeadlineEstimator, LatencyAccumulator, ProbeOutcome};
use serde_json::Value;

fn close(actual: Option<f64>, expected: &Value) -> bool {
    match (actual, expected.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-6,
        (None, None) => expected.is_null(),
        _ => false,
    }
}

#[test]
fn latency_vectors() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("../../../api/latency.testvectors.json")).unwrap();
    let mut failures = 0;
    for c in &cases {
        let mut acc = LatencyAccumulator::default();
        for o in c["outcomes"].as_array().unwrap() {
            if o["break"].as_bool() == Some(true) {
                acc.break_continuity();
                continue;
            }
            if o["timeout"].as_bool() == Some(true) {
                acc.record(ProbeOutcome::Timeout);
                continue;
            }
            let rtt = (o["rttMs"].as_f64().unwrap() * 1e6) as i64;
            acc.record(ProbeOutcome::Reply {
                rtt_nanos: rtt,
                handling_nanos: 0,
            });
        }
        if let Some(expected) = c["deadlineMs"].as_array() {
            let mut estimator = DeadlineEstimator::default();
            let mut deadlines = vec![estimator.deadline_nanos() as f64 / 1e6];
            for o in c["outcomes"].as_array().unwrap() {
                if let Some(rtt) = o["rttMs"].as_f64() {
                    estimator.observe((rtt * 1e6) as u64);
                    deadlines.push(estimator.deadline_nanos() as f64 / 1e6);
                }
            }
            let expected: Vec<f64> = expected.iter().map(|d| d.as_f64().unwrap()).collect();
            if deadlines != expected {
                failures += 1;
                println!("FAIL deadlines: {} -> got {deadlines:?}; want {expected:?}", c["name"]);
            }
        }
        let s = acc.snapshot();
        let e = &c["expect"];
        let ms = |n: Option<u64>| n.map(|n| n as f64 / 1e6);
        let ok = s.count as u64 == e["replies"].as_u64().unwrap()
            && s.timeouts as u64 == e["timeouts"].as_u64().unwrap()
            && close(s.timeout_ratio(), &e["timeoutRatio"])
            && close(ms(s.distribution.map(|d| d.p50)), &e["p50Ms"])
            && close(ms(s.distribution.map(|d| d.p95)), &e["p95Ms"])
            && close(ms(s.jitter), &e["jitterMs"])
            && s.jitter_pairs as u64 == e["jitterPairs"].as_u64().unwrap();
        if ok {
            println!("PASS latency: {}", c["name"]);
        } else {
            failures += 1;
            println!(
                "FAIL latency: {} -> got count={} timeouts={} ratio={:?} p50={:?} p95={:?} jitter={:?} pairs={}; want {e}",
                c["name"],
                s.count,
                s.timeouts,
                s.timeout_ratio(),
                ms(s.distribution.map(|d| d.p50)),
                ms(s.distribution.map(|d| d.p95)),
                ms(s.jitter),
                s.jitter_pairs
            );
        }
    }
    assert_eq!(failures, 0);
}
