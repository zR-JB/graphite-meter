use graphite_meter_proto::{
    discovery::{
        Capabilities, ClientIpSource, DEFAULT_STAGE_LIMIT, IpVersion, LatencyTarget, LatencyTransport, Load,
        NegotiatedProtocol, Preflight, Probe, Protocol, ServerInfo, ThroughputTarget, ThroughputTransport,
    },
    origin::BaseUrl,
    token::{self, SocketTicket},
    upload::Session,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

const PREFLIGHT: &str = include_str!("../../../api/preflight.golden.json");

fn golden(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// `document` as JSON with the member at `pointer` set to `value`, or removed for `None`.
fn edited(document: impl Serialize, pointer: &str, value: Option<Value>) -> Vec<u8> {
    let mut document = serde_json::to_value(document).unwrap();
    let (parent, name) = pointer.rsplit_once('/').unwrap();
    let object = document.pointer_mut(parent).unwrap().as_object_mut().unwrap();
    match value {
        Some(value) => object.insert(name.into(), value),
        None => object.remove(name),
    };
    serde_json::to_vec(&document).unwrap()
}

fn preflight_with(pointer: &str, value: Value) -> Result<Preflight, serde_json::Error> {
    Preflight::decode(&edited(preflight(), pointer, Some(value)))
}

fn probe_with(pointer: &str, value: Value) -> Result<Probe, serde_json::Error> {
    Probe::decode(&edited(probe(), pointer, Some(value)))
}

fn throughput(base_url: &str, transport: ThroughputTransport, protocol: Protocol) -> ThroughputTarget {
    ThroughputTarget {
        base_url: BaseUrl::parse(base_url).unwrap(),
        transport,
        protocol,
    }
}

fn latency(base_url: &str, transport: LatencyTransport) -> LatencyTarget {
    LatencyTarget { base_url: BaseUrl::parse(base_url).unwrap(), transport }
}

/// The preflight `preflight.golden.json` describes.
fn preflight() -> Preflight {
    let tls = "https://speed.example:7249";
    Preflight {
        server: ServerInfo { name: "graphite-meter".into(), location: "fra".into() },
        engine_version: "0.1.0-test".into(),
        generation: "test-generation".into(),
        capabilities: Capabilities {
            upload_checkpoint: true,
            max_stage_ms: Some(300_000),
            throughput: vec![
                throughput("http://speed.example:7246", ThroughputTransport::FetchStream, Protocol::Http1),
                throughput(tls, ThroughputTransport::FetchStream, Protocol::Http3),
                throughput(tls, ThroughputTransport::WebTransport, Protocol::Http3),
                throughput(tls, ThroughputTransport::WebTransportDatagram, Protocol::Http3),
                throughput(".", ThroughputTransport::FetchStream, Protocol::Negotiated),
            ],
            latency: vec![
                latency("http://speed.example:7246", LatencyTransport::WebSocket),
                latency(tls, LatencyTransport::WebTransport),
                latency(".", LatencyTransport::WebSocket),
            ],
        },
    }
}

/// The probe `probe.golden.json` describes.
fn probe() -> Probe {
    Probe {
        client_ip: "198.51.100.4".into(),
        client_ip_version: IpVersion::V4,
        client_ip_source: ClientIpSource::Socket,
        protocol_negotiated: NegotiatedProtocol::Http3,
        load: Some(Load { active: 3, max: 256 }),
    }
}

#[test]
fn a_preflight_encodes_and_decodes_as_its_golden() {
    assert_eq!(serde_json::to_value(preflight()).unwrap(), golden(PREFLIGHT));
    let decoded = Preflight::decode(PREFLIGHT.as_bytes()).unwrap();
    assert_eq!(decoded, preflight());
    assert_eq!(decoded.base_urls().count(), 8, "throughput and latency targets");
}

#[test]
fn a_newer_preflight_keeps_its_known_targets_and_ignores_the_rest() {
    let forward = golden(include_str!("../../../api/preflight.forward.golden.json"));
    let decoded = Preflight::decode(&serde_json::to_vec(&forward["document"]).unwrap()).unwrap();
    assert_eq!(decoded.engine_version, "9.0.0", "never a compatibility test");
    let targets = json!({"throughput": decoded.capabilities.throughput, "latency": decoded.capabilities.latency});
    assert_eq!(targets, forward["decoded"]);
}

#[test]
fn targets_need_their_transport_and_protocol_and_known_ones_a_valid_origin() {
    for target in [
        json!({"baseUrl": ".", "protocol": "http1"}),
        json!({"baseUrl": ".", "transport": "fetch-stream"}),
        json!({"baseUrl": ".", "transport": "masque-stream"}),
        json!({"baseUrl": "https://user@speed.example", "transport": "fetch-stream", "protocol": "http2"}),
        json!({"baseUrl": "https://speed.example/", "transport": "fetch-stream", "protocol": "http2"}),
        json!({"transport": "fetch-stream", "protocol": "http2"}),
    ] {
        assert!(preflight_with("/capabilities/throughput", json!([target])).is_err(), "{target}");
    }
    let unknown = json!({"baseUrl": "not an origin", "transport": "fetch-stream", "protocol": "http4"});
    let decoded = preflight_with("/capabilities/throughput", json!([unknown])).unwrap();
    assert!(decoded.capabilities.throughput.is_empty(), "skipped before its origin is checked");
    let untransported = json!({"baseUrl": ".", "protocol": "ignored"});
    assert!(preflight_with("/capabilities/latency", json!([untransported])).is_err());
    let protocol = json!({"baseUrl": ".", "transport": "websocket", "protocol": 3});
    assert!(
        preflight_with("/capabilities/latency", json!([protocol])).is_ok(),
        "latency protocols are not read"
    );
}

#[test]
fn target_lists_hold_at_most_32_counted_as_sent() {
    let unknown = json!({"baseUrl": ".", "transport": "future", "protocol": "http1"});
    let known = json!({"baseUrl": ".", "transport": "fetch-stream", "protocol": "http1"});
    let mut list = vec![unknown; 31];
    list.push(known.clone());
    let decoded = preflight_with("/capabilities/throughput", json!(list)).unwrap();
    assert_eq!(decoded.capabilities.throughput.len(), 1);
    list.push(known);
    assert!(preflight_with("/capabilities/throughput", json!(list)).is_err());
    let latency = vec![json!({"baseUrl": ".", "transport": "websocket"}); 33];
    assert!(preflight_with("/capabilities/latency", json!(latency)).is_err());
    assert!(preflight_with("/capabilities/latency", Value::Null).is_err());
    assert!(Preflight::decode(&edited(preflight(), "/capabilities/latency", None)).is_err());
}

#[test]
fn metadata_holds_at_most_256_bytes_and_a_generation() {
    let at_limit = "é".repeat(128);
    for field in ["/server/name", "/server/location", "/engineVersion", "/generation"] {
        assert!(preflight_with(field, json!(at_limit)).is_ok(), "{field}");
        assert!(preflight_with(field, json!(format!("{at_limit}x"))).is_err(), "{field}");
    }
    assert!(preflight_with("/generation", json!("")).is_err());
    assert!(Preflight::decode(&edited(preflight(), "/generation", None)).is_err());
    assert!(preflight_with("/engineVersion", json!("")).is_ok());
    for unsafe_text in ["bell\u{7}", "\u{202e}reversed", "line\nbreak"] {
        assert!(preflight_with("/server/location", json!(unsafe_text)).is_err(), "{unsafe_text:?}");
    }
}

#[test]
fn stage_limits_range_from_a_second_to_a_day_and_default_to_five_minutes() {
    let limits = [(999, false), (1000, true), (86_400_000, true), (0, false)];
    for (limit, valid) in limits {
        let decoded = preflight_with("/capabilities/maxStageMs", json!(limit));
        assert_eq!(decoded.is_ok(), valid, "{limit}");
        if let Ok(preflight) = decoded {
            assert_eq!(preflight.capabilities.stage_limit(), Duration::from_millis(limit as u64));
        }
    }
    assert!(preflight_with("/capabilities/maxStageMs", json!(1000.5)).is_err());
    let absent = Preflight::decode(&edited(preflight(), "/capabilities/maxStageMs", None)).unwrap();
    assert_eq!(absent.capabilities.stage_limit(), DEFAULT_STAGE_LIMIT);
    assert_eq!(DEFAULT_STAGE_LIMIT, Duration::from_secs(300));
}

#[test]
fn unknown_members_are_ignored_and_repeated_ones_refused() {
    let added = preflight_with("/capabilities/future", json!({"nested": [1e300, {"x": null}]}));
    assert_eq!(added.unwrap(), preflight());
    let repeated = PREFLIGHT.replacen(r#""generation""#, r#""x": {"a": 1, "a": 2}, "generation""#, 1);
    assert!(Preflight::decode(repeated.as_bytes()).is_err());
}

#[test]
fn a_probe_encodes_and_decodes_as_its_golden() {
    let text = include_str!("../../../api/probe.golden.json");
    assert_eq!(serde_json::to_value(probe()).unwrap(), golden(text));
    assert_eq!(Probe::decode(text.as_bytes()).unwrap(), probe());
    let unloaded = Probe { load: None, ..probe() };
    assert!(!serde_json::to_string(&unloaded).unwrap().contains("load"));
}

#[test]
fn probe_evidence_holds_published_values_only() {
    for (field, value) in [
        ("/clientIp", json!("a")),
        ("/clientIpVersion", json!(6)),
        ("/clientIpSource", json!("forwarded")),
        ("/protocolNegotiated", json!("http/1.1")),
        ("/load/active", json!(0)),
    ] {
        assert!(probe_with(field, value.clone()).is_ok(), "{field} {value}");
    }
    for (field, value) in [
        ("/clientIp", json!("")),
        ("/clientIp", json!("a".repeat(65))),
        ("/clientIpVersion", json!(5)),
        ("/clientIpSource", json!("header")),
        ("/protocolNegotiated", json!("h3-29")),
        ("/load/active", json!(-1)),
        ("/load/max", json!(0)),
        ("/load", json!({"active": 1})),
    ] {
        assert!(probe_with(field, value.clone()).is_err(), "{field} {value}");
    }
    assert_eq!(probe_with("/load", Value::Null).unwrap().load, None);
}

#[test]
fn tokens_and_upload_ids_are_nonempty_and_at_most_8192_bytes() {
    assert!(token::valid("t") && token::valid(&"t".repeat(8192)));
    assert!(!token::valid("") && !token::valid(&"t".repeat(8193)));
    for (id, valid) in [(String::new(), false), ("u".repeat(8192), true), ("u".repeat(8193), false)] {
        let body = serde_json::to_vec(&json!({"uploadId": id})).unwrap();
        assert_eq!(Session::decode(&body).is_ok(), valid, "{} bytes", id.len());
    }
    assert!(Session::decode(br#"{"uploadId":1}"#).is_err());
    let session = Session { upload_id: "abc".into() };
    assert_eq!(serde_json::to_string(&session).unwrap(), r#"{"uploadId":"abc"}"#);
}

#[test]
fn a_socket_ticket_without_authentication_is_empty_and_expired() {
    let unauthenticated = serde_json::to_string(&SocketTicket::unauthenticated()).unwrap();
    assert_eq!(unauthenticated, r#"{"token":"","expires":0}"#);
    let ticket = SocketTicket { token: "t".into(), expires: 1_790_000_000_000 };
    assert_eq!(serde_json::to_string(&ticket).unwrap(), r#"{"token":"t","expires":1790000000000}"#);
}
