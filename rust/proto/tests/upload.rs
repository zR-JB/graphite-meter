use graphite_meter_proto::{
    json,
    refusal::UploadRefusal,
    upload::{Counters, HEARTBEAT, MAX_COUNTER, Record},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vector {
    name: String,
    record: serde_json::Value,
    valid: bool,
}

#[test]
fn records_pass_the_shared_vectors() {
    let vectors: Vec<Vector> =
        serde_json::from_str(include_str!("../../../api/upload-progress.testvectors.json")).unwrap();
    for vector in vectors {
        let decoded = Record::decode(&serde_json::to_vec(&vector.record).unwrap());
        assert_eq!(decoded.is_ok(), vector.valid, "{}: {decoded:?}", vector.name);
        if let Ok(record) = decoded {
            let line = record.line();
            let json = line.strip_suffix('\n').expect("a newline ends each record");
            assert_eq!(Record::decode(json.as_bytes()).ok(), Some(record), "{}", vector.name);
        }
    }
}

#[test]
fn records_encode_as_one_object_and_a_newline() {
    assert_eq!(Record::Ready.line(), "{\"type\":\"ready\"}\n");
    let zeros = Record::Progress(Counters::new(0, 0)).line();
    assert_eq!(zeros, r#"{"type":"progress","bytes":0,"nanos":0}"#.to_owned() + "\n");
    let clamped = Record::Complete(Counters::new(u64::MAX, 5)).line();
    assert_eq!(clamped, format!(r#"{{"type":"complete","bytes":{MAX_COUNTER},"nanos":5}}"#) + "\n");
    let refused = Record::from(UploadRefusal::OwnerMismatch).line();
    let message = r#"{"type":"error","code":"ownerMismatch","message":"upload id belongs to another client"}"#;
    assert_eq!(refused, message.to_owned() + "\n");
    let bare = Record::Error { code: String::new(), message: String::new() };
    assert_eq!(bare.line(), "{\"type\":\"error\"}\n");
    assert_eq!(HEARTBEAT, "\n");
}

#[test]
fn heartbeats_malformed_and_unknown_records_are_no_observations() {
    for line in [
        "",
        " ",
        "{\"type\":\"ready\"} trailing",
        "{\"type\":\"future\",\"bytes\":1,\"nanos\":1}",
        "{\"type\":null}",
        "{\"type\":\"complete\",\"bytes\":1e-1,\"nanos\":0}",
        "{\"type\":\"complete\",\"bytes\":1,\"nanos\":1E16}",
        "{\"type\":\"complete\",\"bytes\":0,\"nanos\":18446744073709551616}",
        "[\"complete\",1,1]",
    ] {
        assert!(Record::decode(line.as_bytes()).is_err(), "{line:?}");
    }
}

#[test]
fn counters_are_integers_in_any_json_spelling() {
    let decoded = Record::decode(br#"{"type":"progress","bytes":1e3,"nanos":2.0}"#).unwrap();
    assert_eq!(decoded, Record::Progress(Counters::new(1000, 2)));
    let checkpoint: Counters = json::decode(br#"{"nanos":7,"bytes":9007199254740991,"extra":[1e400]}"#).unwrap();
    assert_eq!((checkpoint.bytes(), checkpoint.nanos()), (MAX_COUNTER, 7));
    assert!(
        json::decode::<Counters>(br#"{"bytes":1}"#).is_err(),
        "a missing counter differs from zero"
    );
}

#[test]
fn a_regressing_observation_is_stale() {
    let last = Counters::new(100, 1_000);
    assert!(Counters::new(100, 1_000).follows(last), "equal counters repeat");
    assert!(Counters::new(150, 1_100).follows(last));
    assert!(!Counters::new(99, 2_000).follows(last));
    assert!(!Counters::new(200, 999).follows(last), "a complete with older time is stale");
}

#[test]
fn json_objects_refuse_repeated_members_at_any_depth() {
    for raw in [
        r#"{"type":"progress","bytes":1,"bytes":2,"nanos":3}"#,
        r#"{"type":"ready","type":"ready"}"#,
        r#"{"type":"ready","extra":{"field":1,"field":2}}"#,
        r#"{"type":"ready","extra":[{"a":1},{"b":[{"c":1,"c":1}]}]}"#,
        r#"{"type":"error","message":"a","message":"b"}"#,
    ] {
        assert!(Record::decode(raw.as_bytes()).is_err(), "{raw}");
    }
    let separate = r#"{"type":"ready","a":{"x":1},"b":{"x":1},"c":[{"x":1},{"x":1}],"d":"x","x":"{\"x\""}"#;
    assert_eq!(Record::decode(separate.as_bytes()).unwrap(), Record::Ready);
}

#[test]
fn json_objects_refuse_invalid_text_and_other_values() {
    for raw in [
        &b"{\"type\":\"ready\",\"x\":\"\xff\"}"[..],
        br#"{"type":"ready","x":"\ud800"}"#,
        br#"{"type":"ready","x":"\udc00\ud800"}"#,
        b"null",
        b"[]",
        b"\"ready\"",
    ] {
        assert!(Record::decode(raw).is_err(), "{}", String::from_utf8_lossy(raw));
    }
    let nested = format!(r#"{{"type":"ready","x":{}1{}}}"#, "[".repeat(500), "]".repeat(500));
    assert_eq!(Record::decode(nested.as_bytes()).unwrap(), Record::Ready, "unknown members skip unparsed");
}
