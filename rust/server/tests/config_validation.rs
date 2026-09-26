use graphite_meter_server::config::{AuthMode, Config, NativeKind};
use std::{collections::BTreeSet, time::Duration};

type InvalidConfigCase = (&'static str, fn(&mut Config));

fn password() -> Config {
    let mut config = Config {
        advertised_native: Some(BTreeSet::new()),
        ..Config::default()
    };
    config.auth.mode = AuthMode::Password;
    config.auth.public_url = "https://meter.example".into();
    config.auth.password_hash = "test-hash".into();
    config
        .public
        .throughput
        .push("https://meter.example".into());
    config
}

#[test]
fn authentication_constrains_advertised_origins_and_secret_sources() {
    password().validate().unwrap();
    let invalid_cases: [InvalidConfigCase; 9] = [
        ("insecure auth origin", |config| {
            config.auth.public_url = "http://meter.example".into()
        }),
        ("explicit default auth port", |config| {
            config.auth.public_url = "https://meter.example:443".into()
        }),
        ("two password sources", |config| {
            config.auth.password_hash_file = "test-secret-file".into()
        }),
        ("cleartext native advertisement", |config| {
            config.advertised_native = None
        }),
        ("different advertised hostname", |config| {
            config.public.throughput[0] = "https://other.example".into()
        }),
        ("OIDC secret in password mode", |config| {
            config.auth.oidc_client_secret = "test-secret".into()
        }),
        ("missing password source", |config| {
            config.auth.password_hash.clear()
        }),
        ("provider name with control character", |config| {
            config.auth.oidc_provider_name = "invalid\nprovider".into()
        }),
        ("credentials with auth disabled", |config| {
            config.auth.mode = AuthMode::Off
        }),
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
        "https://xn--a.example/realm",
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
fn admission_budgets_share_global_pool_and_timeouts_are_ordered() {
    Config::default().validate().unwrap();
    let invalid_cases: [InvalidConfigCase; 8] = [
        ("client operations exceed global pool", |config| {
            config.limits.operations_per_client = config.limits.operations + 1
        }),
        ("sessions exceed global pool", |config| {
            config.limits.sessions = config.limits.operations + 1
        }),
        ("client sessions exceed client operations", |config| {
            config.limits.sessions_per_client = config.limits.operations_per_client + 1
        }),
        ("client sessions exceed global sessions", |config| {
            config.limits.sessions = config.limits.sessions_per_client - 1
        }),
        ("client connections exceed global connections", |config| {
            config.max_connections_per_client = config.max_connections + 1
        }),
        ("zero operation duration", |config| {
            config.max_operation_duration = Duration::ZERO
        }),
        ("session shorter than operation", |config| {
            config.max_session_duration = config.max_operation_duration - Duration::from_nanos(1)
        }),
        ("zero connection limit", |config| config.max_connections = 0),
    ];
    for (name, invalidate) in invalid_cases {
        let mut config = Config::default();
        invalidate(&mut config);
        assert!(config.validate().is_err(), "accepted {name}");
    }
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
    config
        .public
        .throughput
        .push("https://METER.example:443".into());
    assert!(
        config.validate().is_err(),
        "native and negotiated origin overlap"
    );
    config.public.throughput.clear();
    config.native[NativeKind::H3 as usize].address = ":7249".into();
    config.native[NativeKind::H3 as usize].public_origin = "https://meter.example:443".into();
    assert!(config.validate().is_err(), "deterministic protocol clash");
    config.native[NativeKind::H3 as usize].public_origin = "https://meter.example:7249".into();
    config.validate().unwrap();
    config.native[NativeKind::H3 as usize].address = ":7248".into();
    assert!(config.validate().is_err(), "duplicate bind address");
}
