use graphite_meter_core::{
    failure::{FailureReason, LaneEnding, UploadRefusal},
    route,
    wire::{
        Counters, MAX_UPLOAD_COUNTER, UploadProgress, decode_json, decode_ping, decode_pong, decode_upload_progress,
        encode_ping, encode_pong, encode_upload_progress,
    },
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[test]
fn ping_conforms_to_shared_corpus() {
    for line in include_str!("../../../api/wire.testvectors.txt").lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let [operation, input, expected] = line
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>()
            .try_into()
            .expect("three corpus fields");
        let actual = match operation {
            "encode-ping" => Some(encode_ping(input.parse().unwrap())),
            "encode-pong" => {
                let (id, nanos) = input.split_once(',').unwrap();
                Some(encode_pong(id.parse().unwrap(), nanos.parse().unwrap()))
            }
            "decode-ping" => decode_ping(input).ok().map(|id| id.to_string()),
            "decode-pong" => decode_pong(input)
                .ok()
                .map(|pong| format!("{},{}", pong.id, pong.handling_nanos)),
            other => panic!("unknown corpus operation {other}"),
        };
        if expected == "INVALID" {
            assert_eq!(actual, None, "{operation}: {input}");
        } else {
            assert_eq!(actual.as_deref(), Some(expected), "{operation}: {input}");
        }
    }
}

#[derive(Deserialize)]
struct ProgressCase {
    name: String,
    record: Value,
    valid: bool,
}

#[test]
fn upload_progress_conforms_to_shared_corpus() {
    let cases: Vec<ProgressCase> =
        serde_json::from_str(include_str!("../../../api/upload-progress.testvectors.json")).unwrap();
    for case in cases {
        let raw = serde_json::to_vec(&case.record).unwrap();
        let decoded = decode_upload_progress(&raw);
        assert_eq!(decoded.is_ok(), case.valid, "{}: {decoded:?}", case.name);
        if let Ok(event) = decoded {
            let encoded = encode_upload_progress(&event).unwrap();
            assert_eq!(decode_upload_progress(encoded.as_bytes()), Ok(event), "{}", case.name);
        }
    }
}

#[test]
fn progress_requires_exact_counters_and_rejects_duplicate_fields() {
    for raw in [
        r#"{"type":"progress","bytes":1,"bytes":2,"nanos":3}"#,
        r#"{"type":"ready","\u0074ype":"ready"}"#,
        r#"{"type":"ready","extra":{"field":1,"field":2}}"#,
        r#"{"type":"error","message":"a","message":"b"}"#,
        r#"{"type":"complete","bytes":1e-1,"nanos":0}"#,
        r#"{"type":"complete","bytes":9007199254740992,"nanos":0}"#,
        r#"{"type":"complete","bytes":0,"nanos":-1}"#,
        r#"{"type":"complete","bytes":0,"nanos":null}"#,
        r#"{"type":"complete","bytes":0}"#,
        r#"{"type":"error","code":null}"#,
        r#"{"type":"ready"} trailing"#,
    ] {
        assert!(decode_upload_progress(raw.as_bytes()).is_err(), "{raw}");
    }
    // Go's json/v2 skips what a record's type does not read, unconverted and nested up to 10000 deep.
    let nested = "[".repeat(200) + &"]".repeat(200);
    let ready = format!(r#"{{"type":"ready","extra":1e400,"bytes":1e400,"nested":{nested}}}"#);
    assert_eq!(decode_upload_progress(ready.as_bytes()), Ok(UploadProgress::Ready));
    let event = decode_upload_progress(br#"{"type":"progress","bytes":1e3,"nanos":0.0}"#).unwrap();
    assert_eq!(event, UploadProgress::Progress { bytes: 1000, nanos: 0 });
    // A receiver checkpoint's counters follow the same rule, and only from an object.
    let checkpoint = decode_json::<Counters>(br#"{"bytes":1e3,"nanos":1,"type":"any"}"#);
    assert_eq!(checkpoint.ok(), Some(Counters { bytes: 1000, nanos: 1 }));
    assert!(decode_json::<Counters>(b"[1000,1]").is_err());
    let event = decode_upload_progress(br#"{"type":"complete","bytes":9007199254740991,"nanos":1E+2}"#).unwrap();
    assert_eq!(
        event,
        UploadProgress::Complete {
            bytes: MAX_UPLOAD_COUNTER,
            nanos: 100
        }
    );
}

#[test]
fn progress_encoding_keeps_explicit_zero_and_rejects_inexact_counters() {
    for kind in ["progress", "complete"] {
        let event = if kind == "progress" {
            UploadProgress::Progress { bytes: 0, nanos: 0 }
        } else {
            UploadProgress::Complete { bytes: 0, nanos: 0 }
        };
        let encoded = encode_upload_progress(&event).unwrap();
        let raw: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(raw["bytes"], 0);
        assert_eq!(raw["nanos"], 0);
        let event = if kind == "progress" {
            UploadProgress::Progress {
                bytes: MAX_UPLOAD_COUNTER + 1,
                nanos: 0,
            }
        } else {
            UploadProgress::Complete {
                bytes: MAX_UPLOAD_COUNTER + 1,
                nanos: 0,
            }
        };
        assert!(encode_upload_progress(&event).is_err());
    }
    let encoded = encode_upload_progress(&UploadProgress::Ready).unwrap();
    assert!(!encoded.contains("bytes"));
}

fn pin(text: &'static str) -> Vec<Vec<&'static str>> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('|').map(str::trim).collect())
        .collect()
}

#[test]
fn vocabularies_match_shared_pins_by_name() {
    let reasons = pin(include_str!("../../../api/failurereasons.txt"));
    assert_eq!(reasons.len(), FailureReason::ALL.len());
    for row in reasons {
        let reason = FailureReason::ALL
            .into_iter()
            .find(|reason| reason.name() == row[0])
            .expect("pinned failure reason");
        assert_eq!(reason.label(), row[1]);
    }
    let endings = pin(include_str!("../../../api/laneendings.txt"));
    assert_eq!(endings.len(), LaneEnding::ALL.len());
    for row in endings {
        let ending = LaneEnding::ALL
            .into_iter()
            .find(|ending| ending.name() == row[0])
            .expect("pinned lane ending");
        let fields = [
            ending.websocket_code().to_string(),
            ending.webtransport_code().to_string(),
            ending.reason().into(),
        ];
        assert_eq!(fields, row[1..], "{}", row[0]);
    }
    let refusals = pin(include_str!("../../../api/uploadrefusals.txt"));
    assert_eq!(refusals.len(), UploadRefusal::ALL.len());
    for row in refusals {
        let refusal = UploadRefusal::from_name(row[0]).expect("pinned upload refusal");
        assert_eq!(refusal.message(), row[1]);
        assert_eq!(refusal.status().to_string(), row[2]);
    }
    let failures = pin(include_str!("../../../api/uploadrefusalreasons.txt"));
    assert_eq!(failures.len(), UploadRefusal::ALL.len());
    let pinned: BTreeMap<_, _> = failures.iter().map(|row| (row[0], row[1])).collect();
    let mapped = UploadRefusal::ALL.map(|refusal| (refusal.name(), refusal.failure_reason().name()));
    assert_eq!(pinned, BTreeMap::from(mapped));
}

#[test]
fn routes_match_shared_pin_exactly() {
    let routes = pin(include_str!("../../../api/routes.txt"));
    assert_eq!(routes.len(), route::ALL.len());
    for row in routes {
        let route = route::lookup(row[1]).expect("pinned route is mounted");
        assert_eq!([route.name(), route.path(), route.kind().as_str()], row[..]);
        for near in [format!("{}/", row[1]), format!("{}?x=1", row[1]), row[1].to_uppercase()] {
            assert_eq!(route::lookup(&near), None, "{near}");
        }
    }
    for path in ["", "/", "/%70robe", " /probe", "/probe/child"] {
        assert_eq!(route::lookup(path), None, "{path}");
    }
}
