use graphite_meter_core::wire::{
    MAX_UPLOAD_COUNTER, UploadProgress, decode_ping, decode_pong, decode_upload_progress,
    encode_ping, encode_pong, encode_upload_progress,
};
use serde::Deserialize;
use serde_json::Value;

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
    let cases: Vec<ProgressCase> = serde_json::from_str(include_str!(
        "../../../api/upload-progress.testvectors.json"
    ))
    .unwrap();
    for case in cases {
        let raw = serde_json::to_vec(&case.record).unwrap();
        let decoded = decode_upload_progress(&raw);
        assert_eq!(decoded.is_ok(), case.valid, "{}: {decoded:?}", case.name);
        if let Ok(event) = decoded {
            let encoded = encode_upload_progress(&event).unwrap();
            assert_eq!(
                decode_upload_progress(encoded.as_bytes()),
                Ok(event),
                "{}",
                case.name
            );
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
    let event = decode_upload_progress(br#"{"type":"progress","bytes":1e3,"nanos":0.0}"#).unwrap();
    assert_eq!(
        event,
        UploadProgress::Progress {
            bytes: 1000,
            nanos: 0
        }
    );
    let event =
        decode_upload_progress(br#"{"type":"complete","bytes":9007199254740991,"nanos":1E+2}"#)
            .unwrap();
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
