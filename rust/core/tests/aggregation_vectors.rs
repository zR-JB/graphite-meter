use graphite_meter_core::measurement::*;
use serde_json::Value;

fn close(name: &Value, field: &str, actual: Option<f64>, expected: &Value) {
    let equal = match (actual, expected.as_f64()) {
        (Some(actual), Some(expected)) => (actual - expected).abs() < 1e-6,
        (actual, _) => actual.is_none() && expected.is_null(),
    };
    assert!(equal, "{name}: {field} {actual:?} != {expected}");
}

fn reported(name: &Value, field: &str, result: MeasurementResult, expected: &Value) {
    close(name, field, result.mean_bytes_per_sec, &expected["bytesPerSec"]);
    close(name, field, result.peak_bytes_per_sec, &expected["peakBytesPerSec"]);
}

#[test]
fn shared_aggregation_contract() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("../../../api/aggregation.testvectors.json")).unwrap();
    let text = |value: &Value| value.as_str().unwrap().to_owned();
    let count = |value: &Value| value.as_u64().unwrap();
    for case in cases {
        let name = &case["name"];
        let stage = match case["stage"].as_str().unwrap() {
            "download" => Stage::Download,
            "upload" => Stage::Upload,
            _ => Stage::Bidirectional,
        };
        let mut participants: Vec<String> = case["participants"].as_array().unwrap().iter().map(text).collect();
        let mut engine = AggregateMeasurements::default();
        engine.begin_stage(stage, participants.clone(), 0);
        for value in case["boundaries"].as_array().unwrap() {
            let at_nanos = count(&value["atMs"]) * 1_000_000;
            if let Some(dropout) = value["dropout"].as_array() {
                participants.retain(|id| !dropout.iter().any(|removed| removed.as_str() == Some(id)));
                engine.dropout(&participants, at_nanos);
            }
            let entries = |field: &str| value[field].as_object().into_iter().flatten();
            engine.observe(Boundary {
                at_nanos,
                final_boundary: value["final"] == true,
                down: entries("down").map(|(id, bytes)| (id.clone(), count(bytes))).collect(),
                up: entries("up")
                    .filter(|(_, snapshot)| !snapshot.is_null())
                    .map(|(id, snapshot)| {
                        let snapshot = ReceiverSnapshot {
                            id: text(&snapshot["id"]),
                            bytes: count(&snapshot["bytes"]),
                            nanos: count(&snapshot["nanos"]),
                        };
                        (id.clone(), snapshot)
                    })
                    .collect(),
                observed_up: entries("observedUp")
                    .map(|(id, observed)| {
                        let observed = ObservedUpload {
                            id: text(&observed["id"]),
                            maximum: count(&observed["maximum"]),
                        };
                        (id.clone(), observed)
                    })
                    .collect(),
                ..Boundary::default()
            });
        }
        for (field, direction) in [("down", Direction::Down), ("up", Direction::Up)] {
            let result = engine.result(direction);
            let total = count(&case["totalBytes"][field]);
            assert_eq!(result.total_bytes, total, "{name}: {field} bytes");
            reported(name, field, result, &case["result"][field]);
            for (id, expected) in case["servers"].as_object().into_iter().flatten() {
                let result = engine.server_result(id, direction);
                reported(name, &format!("{id} {field}"), result, &expected[field]);
            }
        }
        let expected = case["intervals"].as_array().unwrap();
        assert_eq!(engine.intervals().len(), expected.len(), "{name}");
        for (interval, expected) in engine.intervals().iter().zip(expected) {
            assert_eq!(interval.reason.name(), expected["reason"], "{name}");
            assert_eq!(interval.complete, expected["complete"], "{name}");
            let window = &expected["window"];
            let Some(actual) = &interval.window else {
                assert!(window.is_null(), "{name}: missing window");
                continue;
            };
            let ms = [actual.start_nanos, actual.end_nanos].map(|nanos| nanos / 1_000_000);
            assert_eq!(ms, [count(&window["startMs"]), count(&window["endMs"])], "{name}");
            close(name, "window", actual.down_bytes_per_sec, &window["downBytesPerSec"]);
            close(name, "window", actual.up_bytes_per_sec, &window["upBytesPerSec"]);
        }
    }
}

#[test]
fn interval_history_stays_bounded_across_stages() {
    let at = |ms: u64, bytes: u64| Boundary {
        at_nanos: ms * 1_000_000,
        down: [("a".to_owned(), bytes)].into(),
        ..Boundary::default()
    };
    let mut engine = AggregateMeasurements::default();
    for i in 0..140 {
        engine.begin_stage(Stage::Download, vec!["a".into()], i * 1_000_000_000);
        engine.observe(at(i * 1000, 0));
        engine.observe(at((i + 1) * 1000, 1000));
    }
    assert_eq!(engine.intervals().len(), MAX_INTERVALS);
    assert_eq!(engine.omitted_intervals(), 12);
    assert_eq!(engine.result(Direction::Down).mean_bytes_per_sec, Some(1000.0));
}
