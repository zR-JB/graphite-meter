use graphite_meter_server::config::{AuthMode, Config, NativeKind};
use std::{collections::BTreeMap, time::Duration};

fn env(values: &[(&str, &str)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
fn failure(values: &[(&str, &str)]) -> String {
    match Config::from_env(&env(values)) {
        Ok(_) => panic!("unexpected accepted configuration"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn defaults_and_presence_are_distinct() {
    let c = Config::from_env(&BTreeMap::new()).unwrap();
    assert_eq!(c.listener(NativeKind::H1).address, ":7246");
    assert!(c.advertised_native.is_none());
    assert!(!c.auth.explicit);
    assert_eq!(c.auth.mode, AuthMode::Off);
    assert!(
        Config::from_env(&env(&[
            ("GM_AUTH_MODE", " off "),
            ("GM_AUTH_UNKNOWN", "ignored")
        ]))
        .is_ok()
    );
    for name in [
        "GM_AUTH_PUBLIC_URL",
        "GM_AUTH_PASSWORD_HASH",
        "GM_AUTH_PASSWORD_HASH_FILE",
        "GM_AUTH_OIDC_ISSUER",
        "GM_AUTH_OIDC_CLIENT_ID",
        "GM_AUTH_OIDC_CLIENT_SECRET",
        "GM_AUTH_OIDC_CLIENT_SECRET_FILE",
        "GM_AUTH_OIDC_PROVIDER_NAME",
        "GM_AUTH_OIDC_ALLOWED_GROUPS",
    ] {
        assert!(failure(&[(name, "")]).contains("authentication"), "{name}");
    }
}

#[test]
fn parses_trimmed_strings_lists_booleans_and_signed_integers() {
    let c = Config::from_env(&env(&[
        ("GM_SERVER_NAME", " meter "),
        ("GM_SERVER_LOCATION", " EU "),
        ("GM_VERBOSE", " TrUe "),
        ("GM_RESULT_HISTORY_DEFAULT", "1"),
        ("GM_MAX_CONNECTIONS", " +1024 "),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "+128"),
        (
            "GM_PUBLIC_ORIGINS",
            " https://meter.example, ,https://other.example ",
        ),
        ("GM_TRUSTED_PROXIES", " 192.0.2.129/24,2001:db8::1/64 "),
    ]))
    .unwrap();
    assert_eq!(c.server_name, "meter");
    assert_eq!(c.server_location, "EU");
    assert!(c.verbose && c.result_history_default);
    assert_eq!(c.max_connections, 1024);
    assert_eq!(c.max_connections_per_client, 128);
    assert_eq!(
        c.public.both,
        ["https://meter.example", "https://other.example"]
    );
    assert_eq!(
        c.trusted_proxies
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["192.0.2.0/24", "2001:db8::/64"]
    );
}

#[test]
fn advertisement_all_none_empty_and_explicit_set() {
    for raw in ["", "none", " , "] {
        let c = Config::from_env(&env(&[
            ("GM_ADVERTISED_NATIVE_ENDPOINTS", raw),
            ("GM_PUBLIC_ORIGINS", "https://meter.example"),
        ]))
        .unwrap();
        assert!(c.advertised_native.unwrap().is_empty());
    }
    assert!(
        Config::from_env(&env(&[("GM_ADVERTISED_NATIVE_ENDPOINTS", " all ")]))
            .unwrap()
            .advertised_native
            .is_none()
    );
    let c = Config::from_env(&env(&[(
        "GM_ADVERTISED_NATIVE_ENDPOINTS",
        "http1-clear,http1-clear",
    )]))
    .unwrap();
    assert_eq!(
        c.advertised_native.unwrap().into_iter().collect::<Vec<_>>(),
        [NativeKind::H1]
    );
    for raw in ["ALL", "all,http1-clear", "http1", "none,http3"] {
        assert!(
            failure(&[("GM_ADVERTISED_NATIVE_ENDPOINTS", raw)])
                .contains("GM_ADVERTISED_NATIVE_ENDPOINTS")
        );
    }
}

#[test]
fn duration_empty_falls_back_but_whitespace_does_not() {
    let c = Config::from_env(&env(&[
        ("GM_MAX_OPERATION_DURATION", ""),
        ("GM_MAX_SESSION_DURATION", "+2h1.5s"),
    ]))
    .unwrap();
    assert_eq!(c.max_operation_duration, Duration::from_secs(300));
    assert_eq!(c.max_session_duration, Duration::from_millis(7_201_500));
    for raw in [" 1s", "1s ", " ", "1", "0", "-1s", "1e3s"] {
        assert!(
            failure(&[("GM_MAX_OPERATION_DURATION", raw)]).contains("GM_MAX_OPERATION_DURATION"),
            "{raw:?}"
        );
    }
}

#[test]
fn invalid_environment_identifies_the_setting() {
    for (name, value) in [
        ("GM_VERBOSE", ""),
        ("GM_VERBOSE", "yes"),
        ("GM_RESULT_HISTORY_DEFAULT", "t"),
        ("GM_MAX_CONNECTIONS", ""),
        ("GM_MAX_CONNECTIONS", "1.5"),
        ("GM_MAX_CONNECTIONS", "9223372036854775808"),
        ("GM_MAX_CONNECTIONS", "-1"),
        ("GM_MAX_ACTIVE_MEASUREMENTS", "0"),
        ("GM_MAX_SESSIONS_PER_CLIENT", "33"),
        ("GM_TRUSTED_PROXIES", "0.0.0.0/0"),
        ("GM_TRUSTED_PROXIES", "::/0"),
        ("GM_TRUSTED_PROXIES", "127.0.0.1"),
        ("GM_TRUSTED_PROXIES", "127.0.0.1/32,"),
        ("GM_H1_ADDR", ""),
        ("GM_AUTH_MODE", "PASSWORD"),
    ] {
        assert!(failure(&[(name, value)]).contains(name), "{name}={value}");
    }
    assert!(
        failure(&[("GM_SERVER_CATALOG", ""), ("GM_SERVER_CATALOG_FILE", "")]).contains("only one")
    );
}
