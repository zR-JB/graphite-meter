//! The shared measurement vectors in `api/`.

use graphite_meter_client::{
    measure::{
        aggregate::{Aggregate, Boundary, Fed, Rate, Reading, Receiver},
        format,
        latency::{Deadline, Latency, ProbeOutcome},
    },
    model::{Direction, Stage},
};
use graphite_meter_proto::{catalog::ServerId, upload::Counters};
use serde_json::Value;
use std::time::{Duration, Instant};

fn cases(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

fn id(text: &str) -> ServerId {
    ServerId::parse(text).unwrap()
}

/// Receivers named in a case, numbered in order of appearance.
#[derive(Default)]
struct Receivers(Vec<String>);

impl Receivers {
    fn number(&mut self, value: &Value) -> u32 {
        let name = value["id"].as_str().unwrap();
        if !self.0.iter().any(|known| known == name) {
            self.0.push(name.to_owned());
        }
        self.0.iter().position(|known| known == name).unwrap() as u32
    }
}

fn boundary(value: &Value, at: Instant, receivers: &mut Receivers) -> Boundary {
    let entries = |field: &str| value[field].as_object().into_iter().flatten();
    let mut readings: Vec<Reading> = Vec::new();
    for (server, bytes) in entries("down") {
        reading(&mut readings, server).down = bytes.as_u64();
    }
    for (server, snapshot) in entries("up").filter(|(_, snapshot)| !snapshot.is_null()) {
        let counters = Counters::new(snapshot["bytes"].as_u64().unwrap(), snapshot["nanos"].as_u64().unwrap());
        reading(&mut readings, server).up = Some(Receiver { id: receivers.number(snapshot), counters });
    }
    for (server, observed) in entries("observedUp") {
        let fed = Fed {
            id: receivers.number(observed),
            bytes: observed["maximum"].as_u64().unwrap(),
        };
        reading(&mut readings, server).fed = Some(fed);
    }
    Boundary { at, stalled: false, last: value["final"] == true, readings }
}

fn reading<'a>(readings: &'a mut Vec<Reading>, server: &str) -> &'a mut Reading {
    if !readings.iter().any(|reading| reading.server.as_str() == server) {
        readings.push(Reading { server: id(server), down: None, up: None, fed: None });
    }
    readings
        .iter_mut()
        .find(|reading| reading.server.as_str() == server)
        .unwrap()
}

/// A vector's headline as mean and peak.
fn expected(value: &Value) -> Option<Rate> {
    let rate = |field: &str| value[field].as_f64();
    Some(Rate { mean: rate("bytesPerSec")?, peak: rate("peakBytesPerSec")? })
}

/// A vector's window as its bounds in milliseconds and its rates.
fn expected_window(value: &Value) -> Option<(u64, u64, Option<f64>, Option<f64>)> {
    let ms = |field: &str| value[field].as_u64();
    Some((
        ms("startMs")?,
        ms("endMs")?,
        value["downBytesPerSec"].as_f64(),
        value["upBytesPerSec"].as_f64(),
    ))
}

/// Rates compare exactly, as Go's vector test compares them.
#[test]
fn aggregation() {
    let base = Instant::now();
    let ms = |value: &Value| base + Duration::from_millis(value.as_u64().unwrap());
    for case in cases(include_str!("../../../api/aggregation.testvectors.json"))
        .as_array()
        .unwrap()
    {
        let name = &case["name"];
        let stage = match case["stage"].as_str().unwrap() {
            "download" => Stage::Download,
            "upload" => Stage::Upload,
            _ => Stage::Bidirectional,
        };
        let named = case["participants"].as_array().unwrap().iter();
        let mut participants: Vec<_> = named.map(|name| id(name.as_str().unwrap())).collect();
        let mut aggregate = Aggregate::new(stage, participants.clone(), base);
        let mut receivers = Receivers::default();
        for value in case["boundaries"].as_array().unwrap() {
            let at = ms(&value["atMs"]);
            if let Some(departed) = value["dropout"].as_array() {
                participants.retain(|id| !departed.iter().any(|gone| gone.as_str() == Some(id.as_str())));
                aggregate.dropout(&participants, at);
            }
            aggregate.observe(boundary(value, at, &mut receivers));
        }
        for (field, direction) in [("down", Direction::Down), ("up", Direction::Up)] {
            assert_eq!(
                aggregate.total(direction),
                case["totalBytes"][field].as_u64().unwrap(),
                "{name}: {field} bytes"
            );
            assert_eq!(aggregate.result(direction), expected(&case["result"][field]), "{name}: {field}");
            for (server, rates) in case["servers"].as_object().into_iter().flatten() {
                assert_eq!(
                    aggregate.server(&id(server), direction),
                    expected(&rates[field]),
                    "{name}: {server} {field}"
                );
            }
        }
        let (intervals, omitted) = aggregate.intervals();
        assert_eq!(omitted, 0);
        let expected = case["intervals"].as_array().unwrap();
        assert_eq!(intervals.len(), expected.len(), "{name}");
        for (interval, expected) in intervals.iter().zip(expected) {
            assert_eq!(interval.reason.name(), expected["reason"], "{name}");
            assert_eq!(interval.complete, expected["complete"], "{name}");
            let window = interval.window.as_ref().map(|window| {
                let ms = |at: Instant| (at - base).as_millis() as u64;
                (ms(window.start), ms(window.end), window.rates.down, window.rates.up)
            });
            assert_eq!(window, expected_window(&expected["window"]), "{name}");
        }
    }
}

#[test]
fn latency() {
    let ms = |value: &Value| Duration::from_secs_f64(value.as_f64().unwrap() / 1000.0);
    for case in cases(include_str!("../../../api/latency.testvectors.json"))
        .as_array()
        .unwrap()
    {
        let name = &case["name"];
        let (mut latency, mut deadline) = (Latency::default(), Deadline::default());
        let mut deadlines = vec![deadline.get()];
        for outcome in case["outcomes"].as_array().unwrap() {
            match () {
                _ if outcome["break"] == true => latency.break_continuity(),
                _ if outcome["timeout"] == true => latency.record(ProbeOutcome::Timeout),
                _ => {
                    let rtt = ms(&outcome["rttMs"]);
                    latency.record(ProbeOutcome::Reply { rtt, handling: Duration::ZERO });
                    deadline.observe(rtt);
                    deadlines.push(deadline.get());
                }
            }
        }
        if let Some(expected) = case["deadlineMs"].as_array() {
            assert_eq!(deadlines, expected.iter().map(ms).collect::<Vec<_>>(), "{name}");
        }
        let summary = latency.summary();
        let ms = |duration: Option<Duration>| {
            duration.map_or(Value::Null, |duration| (duration.as_secs_f64() * 1000.0).into())
        };
        let actual = serde_json::json!({
            "replies": summary.replies,
            "timeouts": summary.timeouts,
            "timeoutRatio": summary.timeout_ratio(),
            "p50Ms": ms(summary.p50),
            "p95Ms": ms(summary.p95),
            "jitterMs": ms(summary.jitter),
            "jitterPairs": summary.jitter_pairs,
        });
        for (field, expected) in case["expect"].as_object().unwrap() {
            let equal = match (actual[field].as_f64(), expected.as_f64()) {
                (Some(actual), Some(expected)) => (actual - expected).abs() < 1e-9,
                _ => actual[field] == *expected,
            };
            assert!(equal, "{name} {field}: {} != {expected}", actual[field]);
        }
    }
}

#[test]
fn formatting() {
    let vectors = cases(include_str!("../../../api/format.testvectors.json"));
    for (kind, cases) in vectors.as_object().unwrap() {
        for case in cases.as_array().unwrap() {
            let value = || case["in"].as_f64().unwrap();
            let actual = match kind.as_str() {
                "ms" => format::ms(value()),
                "latency" => format::latency(value()),
                "added" => format::added(value()),
                "speed" => format::speed(value()),
                "rate" => format::rate(case["bytesPerSec"].as_f64().unwrap()),
                "bytes" => format::bytes(case["in"].as_u64().unwrap()),
                _ => panic!("unknown format {kind}"),
            };
            assert_eq!(actual, case["out"].as_str().unwrap(), "{kind}: {case}");
        }
    }
}
