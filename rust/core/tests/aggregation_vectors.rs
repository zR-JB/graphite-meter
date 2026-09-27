use graphite_meter_core::measurement::*;
use serde_json::Value;

fn close(actual: Option<f64>, expected: &Value) {
    match (actual, expected.as_f64()) {
        (Some(a), Some(e)) => assert!((a - e).abs() < 1e-6, "{a} != {e}"),
        (None, None) => assert!(expected.is_null()),
        _ => panic!("{actual:?} != {expected}"),
    }
}

#[test]
fn shared_aggregation_contract() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../../../api/aggregation.testvectors.json")).unwrap();
    for case in cases {
        let stage = match case["stage"].as_str().unwrap() {
            "download" => Stage::Download,
            "upload" => Stage::Upload,
            _ => Stage::Bidirectional,
        };
        let mut participants: Vec<String> = case["participants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().into())
            .collect();
        let mut engine = AggregateMeasurements::default();
        engine.begin(stage, participants.clone(), 0, IntervalReason::StageStart);
        for value in case["boundaries"].as_array().unwrap() {
            let at_nanos = value["atMs"].as_u64().unwrap() * 1_000_000;
            if let Some(dropout) = value["dropout"].as_array() {
                participants
                    .retain(|id| !dropout.iter().any(|removed| removed.as_str() == Some(id)));
                engine.begin(
                    stage,
                    participants.clone(),
                    at_nanos,
                    IntervalReason::Dropout,
                );
            }
            let mut boundary = Boundary {
                at_nanos,
                final_boundary: value["final"].as_bool().unwrap_or(false),
                ..Boundary::default()
            };
            for (id, count) in value["down"].as_object().unwrap() {
                boundary.down.insert(id.clone(), count.as_u64().unwrap());
            }
            for (id, snapshot) in value["up"].as_object().unwrap() {
                if !snapshot.is_null() {
                    boundary.up.insert(
                        id.clone(),
                        ReceiverSnapshot {
                            id: snapshot["id"].as_str().unwrap().into(),
                            bytes: snapshot["bytes"].as_u64().unwrap(),
                            nanos: snapshot["nanos"].as_u64().unwrap(),
                            requested_at_nanos: 0,
                            received_at_nanos: 0,
                        },
                    );
                }
            }
            engine.observe(boundary);
        }
        for (name, direction) in [("down", Direction::Down), ("up", Direction::Up)] {
            let result = engine.result(stage, direction);
            close(
                result.mean_bytes_per_sec,
                &case["result"][name]["bytesPerSec"],
            );
            close(
                result.peak_bytes_per_sec,
                &case["result"][name]["peakBytesPerSec"],
            );
        }
        let expected = case["intervals"].as_array().unwrap();
        assert_eq!(engine.intervals().len(), expected.len(), "{}", case["name"]);
        for (interval, expected) in engine.intervals().iter().zip(expected) {
            assert_eq!(interval.complete, expected["complete"].as_bool().unwrap());
            let window = interval.window.as_ref().unwrap();
            assert_eq!(
                window.start_nanos,
                expected["window"]["startMs"].as_u64().unwrap() * 1_000_000
            );
            assert_eq!(
                window.end_nanos,
                expected["window"]["endMs"].as_u64().unwrap() * 1_000_000
            );
            close(
                window.down_bytes_per_sec,
                &expected["window"]["downBytesPerSec"],
            );
            close(
                window.up_bytes_per_sec,
                &expected["window"]["upBytesPerSec"],
            );
        }
    }
}
