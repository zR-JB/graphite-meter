use graphite_meter_proto::envelope::{API_VERSION, API_VERSIONS, Envelope};
use serde_json::json;

fn read(document: serde_json::Value) -> Option<Envelope> {
    Envelope::preflight(document.to_string().as_bytes())
}

#[test]
fn the_api_versions_follow_their_pin() {
    let pin = include_str!("../../../api/apiversions.txt");
    let rows: Vec<(u64, String)> = pin
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let (version, first) = line.split_once('|').unwrap();
            (version.trim().parse().unwrap(), first.trim().to_owned())
        })
        .collect();
    let table: Vec<_> = API_VERSIONS
        .iter()
        .map(|(p, v)| (*p, (*v).to_owned()))
        .collect();
    assert_eq!(rows, table);
    assert_eq!(API_VERSIONS.last().unwrap().0, API_VERSION);
}

#[test]
fn any_documents_envelope_is_read_and_a_mismatch_named_both_ways() {
    // A 0.13.4 server's preflight: no API version, no lease, and a capability shape this client may not read.
    let older = read(json!({"engineVersion": "0.13.4", "capabilities": {"future": true}})).unwrap();
    assert_eq!(older.api_version, 1);
    assert_eq!(
        older.mismatch("S2", "0.13.6").as_deref(),
        Some(
            "S2 runs Graphite Meter 0.13.4 (API version 1); this client needs API version 2, from 0.13.5. Update S2."
        )
    );
    let leased = read(json!({"engineVersion": "0.13.5", "lease": {}})).unwrap();
    assert_eq!(
        (leased.api_version, leased.mismatch("S2", "0.13.6")),
        (2, None)
    );
    let newer = read(json!({"engineVersion": "0.15.0", "apiVersion": 3})).unwrap();
    assert_eq!(
        newer.mismatch("S2", "0.13.6").as_deref(),
        Some("S2 runs Graphite Meter 0.15.0, newer than this client (0.13.6); update this client.")
    );
    let unsafe_version = read(json!({"engineVersion": "\u{202e}x", "apiVersion": 2})).unwrap();
    assert_eq!(unsafe_version.engine_version, None);
    let unnamed = read(json!({"apiVersion": 3})).unwrap();
    assert_eq!(
        unnamed.mismatch("S2", "0.13.6").as_deref(),
        Some(
            "S2 runs an unknown Graphite Meter release, newer than this client (0.13.6); update this client."
        )
    );
    for refused in [
        json!([]),
        json!({"server": {}}),
        json!({"engineVersion": "1", "apiVersion": 0}),
        json!({"engineVersion": "1", "apiVersion": -1}),
        json!({"engineVersion": "1", "apiVersion": 2.5}),
        json!({"engineVersion": "1", "apiVersion": "2"}),
    ] {
        assert_eq!(read(refused.clone()), None, "{refused}");
    }
    // Probes from before the envelope carry none.
    let probe = |document: serde_json::Value| Envelope::probe(document.to_string().as_bytes());
    assert_eq!(
        probe(json!({"engineVersion": "0.13.5", "clientIp": "x"})),
        None
    );
    assert_eq!(
        probe(json!({"engineVersion": "0.15.0", "apiVersion": 3}))
            .unwrap()
            .api_version,
        3
    );
}
