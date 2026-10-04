use graphite_meter_server::config::{AuthMode, Config, NativeKind};
use std::collections::BTreeSet;

type InvalidConfigCase = (&'static str, fn(&mut Config));

fn password() -> Config {
    let mut config = Config {
        advertised_native: Some(BTreeSet::new()),
        ..Config::default()
    };
    config.auth.mode = AuthMode::Password;
    config.auth.explicit = true;
    config.auth.public_url = "https://meter.example".into();
    config.auth.password_hash = "test-hash".into();
    config.public.throughput.push("https://meter.example".into());
    config
}

#[test]
fn authentication_constrains_advertised_origins_and_secret_sources() {
    password().validate().unwrap();
    let mut memory = Config {
        max_buffer_bytes: 1024 * 1024,
        max_connections: 512,
        tls_cert: "cert.pem".into(),
        tls_key: "key.pem".into(),
        ..Config::default()
    };
    memory.validate().expect("HTTP/1 connections hold no floor");
    memory.native[NativeKind::H2 as usize].address = ":8444".into();
    let error = memory.validate().unwrap_err().to_string();
    memory.max_buffer_bytes = error
        .strip_prefix("GM_MAX_BUFFER_BYTES (1048576) must be at least ")
        .and_then(|rest| rest.split_once(':')?.0.parse().ok())
        .unwrap_or_else(|| panic!("{error}"));
    memory.validate().expect("HTTP/2 floors fit without the QUIC endpoint");
    memory.native[NativeKind::H3 as usize].address = ":8443".into();
    let error = memory.validate().unwrap_err().to_string();
    assert!(error.contains("QUIC endpoint buffers"), "{error}");
    let invalid_cases: [InvalidConfigCase; 10] = [
        ("insecure auth origin", |config| config.auth.public_url = "http://meter.example".into()),
        ("explicit default auth port", |config| {
            config.auth.public_url = "https://meter.example:443".into()
        }),
        ("two password sources", |config| {
            config.auth.password_hash_file = "test-secret-file".into()
        }),
        ("cleartext native advertisement", |config| config.advertised_native = None),
        ("different advertised hostname", |config| {
            config.public.throughput[0] = "https://other.example".into()
        }),
        ("OIDC secret in password mode", |config| {
            config.auth.oidc_client_secret = "test-secret".into()
        }),
        ("missing password source", |config| config.auth.password_hash.clear()),
        ("provider name with control character", |config| {
            config.auth.oidc_provider_name = "invalid\nprovider".into()
        }),
        ("provider name with bidi control", |config| {
            config.auth.oidc_provider_name = "invalid\u{202e}provider".into()
        }),
        ("credentials with auth disabled", |config| config.auth.mode = AuthMode::Off),
    ];
    for (name, invalidate) in invalid_cases {
        let mut config = password();
        invalidate(&mut config);
        assert!(config.validate().is_err(), "accepted {name}");
    }
    let mut config = Config::default();
    config.auth.explicit = true;
    assert!(config.validate().is_err());
    config.auth.explicit = false;
    config.validate().unwrap();
}

#[test]
fn oidc_requires_complete_https_provider_and_allows_issuer_path() {
    let mut config = password();
    config.auth.mode = AuthMode::Hybrid;
    config.auth.oidc_issuer = "https://identity.example/realms/graphite".into();
    config.auth.oidc_client_id = "meter".into();
    config.auth.oidc_client_secret = "test-secret".into();
    config.auth.oidc_allowed_groups = vec!["operators".into()];
    config.validate().unwrap();
    for issuer in [
        "https://@identity.example",
        "http://identity.example",
        "https://identity.example?x=1",
        "https://identity.example/#fragment",
        "https://identity.\nexample",
        "https://1.2.3/realm",
        "https://BÜCHER.example/realm",
        "https://identity.example/réalm",
    ] {
        let mut invalid = config.clone();
        invalid.auth.oidc_issuer = issuer.into();
        assert!(invalid.validate().is_err(), "accepted {issuer}");
    }
    config.auth.oidc_allowed_groups.clear();
    assert!(config.validate().is_err());
}

#[test]
fn listeners_and_advertisements_cannot_claim_conflicting_protocols() {
    let mut config = Config::default();
    config.native[NativeKind::H2 as usize].address = ":7248".into();
    assert!(config.validate().is_err(), "TLS files required");
    config.tls_cert = "test-cert.pem".into();
    config.tls_key = "test-key.pem".into();
    config.native[NativeKind::H2 as usize].public_origin = "https://meter.example".into();
    config.validate().unwrap();
    config.public.throughput.push("https://METER.example:443".into());
    assert!(config.validate().is_err(), "native and negotiated origin overlap");
    config.public.throughput.clear();
    config.native[NativeKind::H3 as usize].address = ":7249".into();
    config.native[NativeKind::H3 as usize].public_origin = "https://meter.example:443".into();
    assert!(config.validate().is_err(), "deterministic protocol clash");
    config.native[NativeKind::H3 as usize].public_origin = "https://meter.example:7249".into();
    config.validate().unwrap();
    config.native[NativeKind::H3 as usize].address = ":7248".into();
    assert!(config.validate().is_err(), "duplicate bind address");
}
