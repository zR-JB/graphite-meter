use graphite_meter_core::{catalog::ServerEntry, discovery::*};
use serde_json::{Value, json};

const PREFLIGHT: &[u8] = include_bytes!("../../../api/preflight.golden.json");
const PROBE: &[u8] = include_bytes!("../../../api/probe.golden.json");
const FORWARD: &[u8] = include_bytes!("../../../api/preflight.forward.golden.json");
fn decode_preflight_value(value: &Value) -> Result<Preflight, DiscoveryError> {
    Preflight::decode(&serde_json::to_vec(value).unwrap())
}
fn decode_probe_value(value: &Value) -> Result<Probe, DiscoveryError> {
    Probe::decode(&serde_json::to_vec(value).unwrap())
}

#[test]
fn shared_goldens_round_trip_without_local_fields() {
    let preflight = Preflight::decode(PREFLIGHT).unwrap();
    assert_eq!(
        serde_json::to_value(&preflight).unwrap(),
        serde_json::from_slice::<Value>(PREFLIGHT).unwrap()
    );
    assert_eq!(
        preflight.capabilities.throughput[3].transport,
        ThroughputTransport::WebTransportDatagram
    );
    let evidence = Probe::decode(PROBE).unwrap();
    assert_eq!(
        serde_json::to_value(&evidence).unwrap(),
        serde_json::from_slice::<Value>(PROBE).unwrap()
    );
    assert_eq!(evidence.protocol_negotiated, ProtocolNegotiated::Http3);
}

#[test]
fn rejects_duplicates_even_in_unknown_nested_fields_but_allows_additions() {
    let fixture = std::str::from_utf8(PREFLIGHT).unwrap();
    let with = |suffix: &str| format!("{}{suffix}}}", fixture.trim_end().strip_suffix('}').unwrap());
    let (head, tail) = fixture.rsplit_once(r#""baseUrl""#).unwrap();
    let deep = "[".repeat(200) + &"]".repeat(200);
    for (raw, accepted) in [
        (with(r#", "future":{"x":1,"x":2}"#), false),
        (with(r#", "future":[{"x":1,"x":2}]"#), false),
        (with(r#", "generation":"duplicate""#), false),
        // Go's json/v2 skips unknown members unconverted, nested up to 10000 deep, a target's too.
        (with(r#", "future":1e400"#), true),
        (with(&format!(r#", "future":{deep}"#)), true),
        (fixture.replacen(r#""baseUrl""#, r#""future":1e400,"baseUrl""#, 1), true),
        (format!(r#"{head}"protocol":1e400,"baseUrl"{tail}"#), true),
    ] {
        assert_eq!(Preflight::decode(raw.as_bytes()).is_ok(), accepted, "{raw}");
    }
    let mut value: Value = serde_json::from_slice(PREFLIGHT).unwrap();
    value["future"] = json!({"nested": [1, 2]});
    value["server"]["future"] = json!(true);
    value["capabilities"]["throughput"][0]["future"] = json!({"supported": true});
    decode_preflight_value(&value).unwrap();
    let duplicate_unknown_field = br#"{
        "clientIp": "x",
        "clientIpVersion": 4,
        "clientIpSource": "socket",
        "protocolNegotiated": "h2",
        "future": {"x": 0, "x": 1}
    }"#;
    assert!(Probe::decode(duplicate_unknown_field).is_err());
}

#[test]
fn metadata_and_target_lists_are_bounded_in_bytes() {
    let base: Value = serde_json::from_slice(PREFLIGHT).unwrap();
    for field in ["engineVersion", "generation"] {
        let mut value = base.clone();
        value[field] = json!("é".repeat(128));
        decode_preflight_value(&value).unwrap();
        value[field] = json!("é".repeat(129));
        assert!(decode_preflight_value(&value).is_err());
    }
    for field in ["name", "location"] {
        let mut value = base.clone();
        value["server"][field] = json!("x".repeat(257));
        assert!(decode_preflight_value(&value).is_err());
    }
    for list in ["throughput", "latency"] {
        let mut value = base.clone();
        let target = value["capabilities"][list][0].clone();
        value["capabilities"][list] = json!(vec![target.clone(); 32]);
        decode_preflight_value(&value).unwrap();
        value["capabilities"][list] = json!(vec![target; 33]);
        assert!(decode_preflight_value(&value).is_err());
        value["capabilities"][list] = Value::Null;
        assert!(decode_preflight_value(&value).is_err());
        value["capabilities"].as_object_mut().unwrap().remove(list);
        assert!(decode_preflight_value(&value).is_err());
    }
    let minimal = json!({
        "generation": "g",
        "capabilities": {
            "throughput": [],
            "latency": []
        }
    });
    decode_preflight_value(&minimal).unwrap();
    let mut no_generation = minimal;
    no_generation["generation"] = json!("");
    assert!(decode_preflight_value(&no_generation).is_err());
}

#[test]
fn validates_origins_and_transport_strings() {
    let base: Value = serde_json::from_slice(PREFLIGHT).unwrap();
    for raw in [
        "https://user@host",
        "https://host/path",
        "https://host?",
        "https://host#",
        "https://host:65536",
        "ftp://host",
        "",
    ] {
        let mut value = base.clone();
        value["capabilities"]["throughput"][0]["baseUrl"] = json!(raw);
        assert!(decode_preflight_value(&value).is_err(), "{raw}");
    }
    for (list, field, text) in [
        ("throughput", "transport", "websocket"),
        ("latency", "transport", "fetch-stream"),
        ("throughput", "protocol", "h3"),
    ] {
        let mut value = base.clone();
        value["capabilities"][list][0][field] = json!(text);
        let decoded = decode_preflight_value(&value).unwrap();
        let count = if list == "throughput" {
            decoded.capabilities.throughput.len()
        } else {
            decoded.capabilities.latency.len()
        };
        assert_eq!(count, base["capabilities"][list].as_array().unwrap().len() - 1);
    }
    let mut direct = Preflight::decode(PREFLIGHT).unwrap();
    direct.capabilities.latency[0].base_url = "https://host/path".into();
    assert_eq!(direct.validate(), Err(DiscoveryError::InvalidOrigin));
}

#[test]
fn probe_evidence_and_occupancy_preserve_go_bounds() {
    let base: Value = serde_json::from_slice(PROBE).unwrap();
    for (field, value) in [
        ("clientIp", json!("")),
        ("clientIp", json!("x".repeat(65))),
        ("clientIpVersion", json!(5)),
        ("clientIpSource", json!("header")),
        ("protocolNegotiated", json!("http3")),
    ] {
        let mut invalid_probe = base.clone();
        invalid_probe[field] = value;
        assert!(decode_probe_value(&invalid_probe).is_err());
    }
    for load in [
        json!({"active": -1, "max": 1}),
        json!({"active": 0, "max": 0}),
        json!({"active": u64::MAX, "max": 1}),
    ] {
        let mut value = base.clone();
        value["load"] = load;
        assert!(decode_probe_value(&value).is_err());
    }
    let mut value = base.clone();
    value["load"] = json!({"active": 9, "max": 1});
    decode_probe_value(&value).unwrap();
    value["load"] = Value::Null;
    let decoded = decode_probe_value(&value).unwrap();
    assert!(serde_json::to_value(decoded).unwrap().get("load").is_none());
    // Go validates evidence bounds, not the address's syntax or inferred family.
    value["clientIp"] = json!("unknown-client");
    value["clientIpVersion"] = json!(6);
    decode_probe_value(&value).unwrap();
}

#[test]
fn catalogue_checks_all_discovery_lanes() {
    let entry = ServerEntry {
        url: "https://speed.example".into(),
        ..ServerEntry::default()
    };
    let mut preflight = Preflight::decode(PREFLIGHT).unwrap();
    entry.validate_discovery(&preflight).unwrap();
    preflight.capabilities.latency[0].base_url = "https://other.example".into();
    assert_eq!(
        entry.validate_discovery(&preflight),
        Err(DiscoveryError::UnapprovedOrigin)
    );
    let allowed = ServerEntry {
        additional_origins: vec!["https://other.example".into()],
        ..entry
    };
    allowed.validate_discovery(&preflight).unwrap();
    preflight.generation.clear();
    assert_eq!(
        allowed.validate_discovery(&preflight),
        Err(DiscoveryError::InvalidMetadata)
    );
}

#[test]
fn newer_servers_targets_are_skipped_but_counted_as_sent() {
    let golden: Value = serde_json::from_slice(FORWARD).unwrap();
    let document = &golden["document"];
    let decoded = decode_preflight_value(document).unwrap().capabilities;
    assert_eq!(
        json!({"throughput": decoded.throughput, "latency": decoded.latency}),
        golden["decoded"]
    );
    for list in ["throughput", "latency"] {
        let mut value = document.clone();
        let targets = value["capabilities"][list].as_array_mut().unwrap();
        let skipped = targets[1].clone();
        targets.resize(32, skipped.clone());
        decode_preflight_value(&value).unwrap();
        value["capabilities"][list].as_array_mut().unwrap().push(skipped);
        assert!(decode_preflight_value(&value).is_err(), "{list}");
    }
    for (list, field) in [
        ("throughput", "transport"),
        ("throughput", "protocol"),
        ("latency", "transport"),
    ] {
        let mut empty = document.clone();
        empty["capabilities"][list][1][field] = json!("");
        let mut missing = document.clone();
        missing["capabilities"][list][1].as_object_mut().unwrap().remove(field);
        for value in [empty, missing] {
            assert!(decode_preflight_value(&value).is_err(), "{list} {field}");
        }
    }
    let mut known = document.clone();
    known["capabilities"]["throughput"][0]["baseUrl"] = document["capabilities"]["throughput"][1]["baseUrl"].clone();
    assert!(decode_preflight_value(&known).is_err());
}
