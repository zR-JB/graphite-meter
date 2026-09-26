use graphite_meter_core::discovery::{LatencyTransport, Protocol, ThroughputTransport};
use graphite_meter_server::{
    config::{AuthMode, Config, NativeKind},
    preflight::Preflight,
};
use std::{collections::BTreeSet, sync::Arc};

#[test]
fn native_and_public_discovery_match_shared_golden() {
    let mut config = Config {
        server_location: "fra".into(),
        engine_version: "0.1.0-test".into(),
        ..Config::default()
    };
    config.native[NativeKind::H3 as usize].address = ":7249".into();
    config.tls_cert = "test-cert.pem".into();
    config.tls_key = "test-key.pem".into();
    config.public.both.push("self".into());
    let preflight = Preflight::new(Arc::new(config)).unwrap();
    let document = preflight.build("speed.example:7246").unwrap();
    let mut expected: serde_json::Value =
        serde_json::from_str(include_str!("../../../api/preflight.golden.json")).unwrap();
    expected["generation"] = document.generation.clone().into();
    expected["implementation"] = "rust".into();
    expected["capabilities"]["uploadCheckpoint"] = true.into();
    assert_eq!(serde_json::to_value(&document).unwrap(), expected);
    assert_eq!(document.generation.len(), 32);
    assert_eq!(
        preflight.build("other.example").unwrap().generation,
        document.generation
    );
}

#[test]
fn authentication_does_not_disable_configured_webtransport() {
    let mut config = Config {
        advertised_native: Some(BTreeSet::from([NativeKind::H3])),
        ..Config::default()
    };
    config.native[NativeKind::H3 as usize].address = ":7249".into();
    config.native[NativeKind::H3 as usize].public_origin = "https://meter.example:7249".into();
    config.tls_cert = "test-cert.pem".into();
    config.tls_key = "test-key.pem".into();
    config.auth.mode = AuthMode::Password;
    config.auth.public_url = "https://meter.example".into();
    config.auth.password_hash = "test-hash".into();
    let document = Preflight::new(Arc::new(config))
        .unwrap()
        .build("meter.example")
        .unwrap();
    assert_eq!(document.capabilities.throughput.len(), 3);
    assert!(
        document
            .capabilities
            .throughput
            .iter()
            .any(|target| target.transport == ThroughputTransport::WebTransport)
    );
    assert_eq!(
        document.capabilities.latency[0].transport,
        LatencyTransport::WebTransport
    );
}

#[test]
fn public_roles_merge_default_ports_and_csp_includes_socket_schemes() {
    let mut config = Config {
        advertised_native: Some(BTreeSet::new()),
        ..Config::default()
    };
    config.public.both = vec!["self".into(), "https://meter.example".into()];
    config.public.throughput = vec!["https://METER.example:443".into()];
    config.public.latency = vec!["https://meter.example:443".into()];
    let preflight = Preflight::new(Arc::new(config)).unwrap();
    let document = preflight.build("meter.example").unwrap();
    assert_eq!(document.capabilities.throughput.len(), 2);
    assert_eq!(document.capabilities.latency.len(), 2);
    assert!(
        document
            .capabilities
            .throughput
            .iter()
            .all(|target| target.protocol == Protocol::Negotiated)
    );
    assert_eq!(
        preflight.connect_origins("meter.example").unwrap(),
        ["https://meter.example", "wss://meter.example"]
    );
}

#[test]
fn ipv6_request_authority_advertises_valid_bracketed_origins() {
    let preflight = Preflight::new(Arc::new(Config::default())).unwrap();
    let document = preflight.build("[2001:db8::1]:7246").unwrap();
    assert_eq!(
        document.capabilities.throughput[0].base_url,
        "http://[2001:db8::1]:7246"
    );
    assert!(preflight.build("user@meter.example").is_err());
}
