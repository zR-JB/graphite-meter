use graphite_meter_server::config::{self, AuthMode, ValidatedConfig};
use std::{collections::BTreeMap, ffi::OsString, process::Command, time::Duration};

fn load(env: &[(&str, &str)], args: &[&str]) -> Result<ValidatedConfig, String> {
    let env: BTreeMap<_, _> = env.iter().map(|(key, value)| (*key, OsString::from(value))).collect();
    let args: Vec<_> = args.iter().map(OsString::from).collect();
    config::load(|name| env.get(name).cloned(), &args, &mut std::io::sink())
        .map(|config| config.expect("configuration, not usage"))
        .map_err(|error| error.to_string())
}

fn failure(env: &[(&str, &str)], args: &[&str]) -> String {
    load(env, args).err().expect("configuration accepted")
}

#[test]
fn defaults_and_presence_are_distinct() {
    let c = load(&[("GM_SERVER_NAME", "environment")], &["-name", "edge"]).unwrap();
    assert_eq!(c.server_name, "edge");
    assert_eq!(c.auth.mode, AuthMode::Off);
    let c = load(&[("GM_MAX_STAGE_DURATION", "3h")], &[]).unwrap();
    let lifetimes = (c.max_operation_duration.as_secs(), c.max_session_duration.as_secs());
    assert_eq!(lifetimes, (10_860, 10_860));
    let c = load(&[("GM_MAX_STAGE_DURATION", "2h")], &["-max-stage-duration=24h"]).unwrap();
    assert_eq!(c.max_stage_duration, Duration::from_secs(86_400));
    for duration in ["999ms", "25h"] {
        let refused = failure(&[], &[&format!("-max-stage-duration={duration}")]);
        assert_eq!(refused, "GM_MAX_STAGE_DURATION must be from 1s to 24h");
    }
    assert!(load(&[("GM_AUTH_MODE", " off "), ("GM_AUTH_UNKNOWN", "ignored")], &[]).is_ok());
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
        assert!(failure(&[(name, "")], &[]).contains("authentication"), "{name}");
    }
    for flag in ["-auth-public-url=", "-auth-oidc-allowed-groups=", "-auth-oidc-provider-name=Authelia"] {
        assert!(failure(&[], &[flag]).contains("authentication"), "{flag}");
    }
}

#[test]
fn invalid_settings_name_what_failed() {
    let long_location = "a".repeat(257);
    for (name, value) in [
        ("GM_MAX_CONNECTIONS", "9223372036854775808"),
        ("GM_MAX_ACTIVE_MEASUREMENTS", "0"),
        ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "257"),
        ("GM_MAX_ACTIVE_SESSIONS", "257"),
        ("GM_MAX_ACTIVE_SESSIONS", "7"),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "4097"),
        ("GM_MAX_CONNECTIONS", "0"),
        ("GM_MAX_SESSION_DURATION", "359999999999ns"),
        ("GM_PUBLIC_ORIGINS", "https://meter.example:0"),
        ("GM_MAX_SESSIONS_PER_CLIENT", "33"),
        ("GM_MAX_OPERATION_DURATION", "-1s"),
        ("GM_TRUSTED_PROXIES", "0.0.0.0/0"),
        ("GM_TRUSTED_PROXIES", "10.0.0.0/8,::/0"),
        ("GM_TRUSTED_PROXIES", "127.0.0.1"),
        ("GM_TRUSTED_PROXIES", "010.0.0.0/8"),
        ("GM_TRUSTED_PROXIES", "10.0.0.0/08"),
        ("GM_H1_ADDR", ""),
        ("GM_AUTH_MODE", "PASSWORD"),
        ("GM_SERVER_NAME", "bad\u{1b}[31mname"),
        ("GM_SERVER_LOCATION", &long_location),
        ("GM_SERVER_CATALOG_FILE", "relative.json"),
        ("GM_SERVER_CATALOG_FILE", "/nonexistent/../catalog.json"),
        ("GM_SERVER_CATALOG_FILE", "/nonexistent-catalog.json"),
    ] {
        assert!(failure(&[(name, value)], &[]).contains(name), "{name}={value}");
    }
    let huge = [
        ("GM_MAX_ACTIVE_MEASUREMENTS", "5000000000"),
        ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "5000000000"),
    ];
    load(&huge, &[]).expect("without HTTP/3, Go takes limits past a QUIC stream count");
    assert!(failure(&[("GM_SERVER_CATALOG", ""), ("GM_SERVER_CATALOG_FILE", "")], &[]).contains("only one"));
    assert!(failure(&[("GM_MAX_CONNECTIONS", "many")], &["-max-connections=9"]).contains("GM_MAX_CONNECTIONS"));
    assert!(failure(&[], &["-not-a-flag"]).contains("flag provided but not defined"));
    assert!(failure(&[], &["-max-connections", "-5"]).contains("must be greater than zero"));
}

#[test]
fn the_auth_mode_is_checked_once_flags_apply_as_in_go() {
    let config = load(&[("GM_AUTH_MODE", "bogus")], &["-auth-mode", "off"]).unwrap();
    assert_eq!(config.auth.mode, AuthMode::Off);
    for (env, args) in [
        (&[("GM_AUTH_MODE", "bogus")][..], &[][..]),
        (&[("GM_AUTH_MODE", "")], &[]),
        (&[], &["-auth-mode=bogus"]),
    ] {
        let refused = failure(env, args);
        assert_eq!(refused, "GM_AUTH_MODE must be off, password, oidc, or hybrid", "{env:?} {args:?}");
    }
}

#[test]
fn the_authentication_origin_is_canonical_once_validated() {
    let env = [
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", "HTTPS://Meter.Example:8443"),
        (
            "GM_AUTH_PASSWORD_HASH",
            "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0",
        ),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"),
        ("GM_PUBLIC_ORIGINS", "self"),
    ];
    assert_eq!(load(&env, &[]).unwrap().auth.public_url, "https://meter.example:8443");
    // As a browser writes the origin: a port without leading zeros, and none where it is the default.
    for (public, canonical) in [(":08443", ":8443"), (":0443", "")] {
        let public = format!("https://meter.example{public}");
        let env = [env.as_slice(), &[("GM_AUTH_PUBLIC_URL", public.as_str())]].concat();
        let canonical = format!("https://meter.example{canonical}");
        assert_eq!(load(&env, &[]).unwrap().auth.public_url, canonical);
    }
}

#[test]
fn executable_refuses_a_hostile_identity() {
    let result = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .env("GM_H1_ADDR", "127.0.0.1:0")
        .env("GM_SERVER_NAME", "bad\u{1b}[31mname")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid catalogue server identity"));
}
