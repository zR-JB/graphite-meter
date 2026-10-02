use graphite_meter_server::config::{self, AuthMode, NativeKind, ValidatedConfig};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};

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
    assert_eq!(c.listener(NativeKind::H1).address, ":7246");
    assert!(c.advertised_native.is_none());
    assert!(!c.auth.explicit);
    assert_eq!(c.auth.mode, AuthMode::Off);
    assert_eq!(c.max_operation_duration, Duration::from_secs(360));
    for (env, operation, session) in [
        (vec![("GM_MAX_STAGE_DURATION", "3h")], 10_860, 10_860),
        (
            vec![
                ("GM_MAX_STAGE_DURATION", "3h"),
                ("GM_MAX_OPERATION_DURATION", "20s"),
                ("GM_MAX_SESSION_DURATION", "1h"),
            ],
            20,
            3600,
        ),
        (
            vec![("GM_MAX_STAGE_DURATION", "3h"), ("GM_MAX_OPERATION_DURATION", "4h")],
            14_400,
            14_400,
        ),
    ] {
        let c = load(&env, &[]).unwrap();
        assert_eq!(
            (c.max_operation_duration.as_secs(), c.max_session_duration.as_secs()),
            (operation, session)
        );
    }
    assert_eq!(
        load(&[("GM_MAX_STAGE_DURATION", "2h")], &["-max-stage-duration=24h"])
            .unwrap()
            .max_stage_duration,
        Duration::from_secs(86_400)
    );
    for duration in ["999ms", "25h"] {
        assert_eq!(
            failure(&[], &[&format!("-max-stage-duration={duration}")]),
            "GM_MAX_STAGE_DURATION must be from 1s to 24h"
        );
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
    for flag in [
        "-auth-public-url=",
        "-auth-oidc-allowed-groups=",
        "-auth-oidc-provider-name=Authelia",
    ] {
        assert!(failure(&[], &[flag]).contains("authentication"), "{flag}");
    }
}

#[test]
fn invalid_settings_name_what_failed() {
    let long_location = "a".repeat(257);
    for (name, value) in [
        ("GM_VERBOSE", "yes"),
        ("GM_RESULT_HISTORY_DEFAULT", "on"),
        ("GM_MAX_CONNECTIONS", "1.5"),
        ("GM_MAX_CONNECTIONS", "0x10"),
        ("GM_MAX_CONNECTIONS", "9223372036854775808"),
        ("GM_MAX_CONNECTIONS", "-1"),
        ("GM_MAX_ACTIVE_MEASUREMENTS", "0"),
        ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "257"),
        ("GM_MAX_ACTIVE_SESSIONS", "257"),
        ("GM_MAX_ACTIVE_SESSIONS", "7"),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "4097"),
        ("GM_MAX_CONNECTIONS", "0"),
        ("GM_MAX_SESSION_DURATION", "359999999999ns"),
        ("GM_PUBLIC_ORIGINS", "https://meter.example:0"),
        ("GM_MAX_SESSIONS_PER_CLIENT", "33"),
        ("GM_MAX_OPERATION_DURATION", "1"),
        ("GM_MAX_OPERATION_DURATION", "0"),
        ("GM_MAX_OPERATION_DURATION", "-1s"),
        ("GM_MAX_OPERATION_DURATION", "1e3s"),
        ("GM_TRUSTED_PROXIES", "0.0.0.0/0"),
        ("GM_TRUSTED_PROXIES", "10.0.0.0/8,::/0"),
        ("GM_TRUSTED_PROXIES", "127.0.0.1"),
        ("GM_TRUSTED_PROXIES", "010.0.0.0/8"),
        ("GM_TRUSTED_PROXIES", "10.0.0.0/08"),
        ("GM_H1_ADDR", ""),
        ("GM_AUTH_MODE", "PASSWORD"),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "all,http1-clear"),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1"),
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
    assert!(failure(&[("GM_SERVER_CATALOG_FILE", "/nonexistent-catalog.json")], &[]).contains("/nonexistent-catalog"));
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
        assert_eq!(
            failure(env, args),
            "GM_AUTH_MODE must be off, password, oidc, or hybrid",
            "{env:?} {args:?}"
        );
    }
}

fn server() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"));
    command
        .env_clear()
        .env("GM_H1_ADDR", "127.0.0.1:0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Runs the server until it exits or reports a listener: the exit code, or None once it serves, and stderr.
fn start(env: &[(&str, &str)], args: &[&str]) -> (Option<i32>, String) {
    let mut child = server().envs(env.iter().copied()).args(args).spawn().unwrap();
    let mut stderr = String::new();
    for line in BufReader::new(child.stderr.take().unwrap())
        .lines()
        .map_while(Result::ok)
    {
        stderr += &line;
        stderr += "\n";
        if line.contains(" listening on ") {
            child.kill().unwrap();
            child.wait().unwrap();
            return (None, stderr);
        }
    }
    (child.wait().unwrap().code(), stderr)
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
        assert_eq!(
            load(&env, &[]).unwrap().auth.public_url,
            format!("https://meter.example{canonical}")
        );
    }
    let (code, stderr) = start(&env, &[]);
    assert_eq!(code, None, "{stderr}");
    assert!(stderr.contains(" origin=https://meter.example:8443 "), "{stderr}");
}

#[test]
fn executable_reports_usage_and_refuses_invalid_identity_like_go() {
    let help = server().arg("-h").output().unwrap();
    let usage = String::from_utf8(help.stderr).unwrap();
    assert!(help.status.success() && help.stdout.is_empty());
    assert!(usage.contains("hash-password") && usage.contains("-h1-addr"));
    assert!(!usage.contains("GM_AUTH_PASSWORD_HASH)") && !usage.contains("GM_TRUSTED_PROXIES"));
    let password = [
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", "https://meter.example"),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"),
        ("GM_PUBLIC_ORIGINS", "https://meter.example"),
        ("GM_AUTH_PASSWORD_HASH_FILE", "/nonexistent-hash"),
    ];
    let tls = [
        ("GM_H2_ADDR", "127.0.0.2:0"),
        ("GM_TLS_CERT", "/nonexistent-cert.pem"),
        ("GM_TLS_KEY", "/k.pem"),
    ];
    for (env, args, expected) in [
        (
            &[("GM_SERVER_NAME", "bad\u{1b}[31mname")][..],
            &[][..],
            "configuration error: \"GM_SERVER_NAME, GM_SERVER_LOCATION or the server catalogue: \
             invalid catalogue server identity\"",
        ),
        (
            &[],
            &["-max-connections-per-client=010", "-max-connections=9"],
            "must not exceed",
        ),
        (&[], &["-nope"], "flag provided but not defined: -nope\nUsage:\n"),
        (
            &tls,
            &[],
            "server error: \"load matching TLS certificate/key: open /nonexistent-cert.pem: no such file or directory\"",
        ),
        (
            &password,
            &[],
            "server error: \"password hash: open /nonexistent-hash: no such file or directory\"",
        ),
    ] {
        let (code, stderr) = start(env, args);
        assert_eq!(code, Some(1), "{stderr}");
        assert!(stderr.contains(expected), "{expected:?} missing from\n{stderr}");
    }
}
