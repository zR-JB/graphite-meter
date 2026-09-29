use graphite_meter_server::config::{self, AuthMode, Config, NativeKind, ValidatedConfig};
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
    let c = load(&[], &[]).unwrap();
    assert_eq!(c.listener(NativeKind::H1).address, ":7246");
    assert!(c.advertised_native.is_none());
    assert!(!c.auth.explicit);
    assert_eq!(c.auth.mode, AuthMode::Off);
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
fn parses_like_go_settings() {
    let c = load(
        &[
            ("GM_SERVER_NAME", " meter "),
            ("GM_SERVER_LOCATION", " EU "),
            ("GM_VERBOSE", " TrUe "),
            ("GM_RESULT_HISTORY_DEFAULT", "1"),
            ("GM_MAX_CONNECTIONS", " +1024 "),
            ("GM_MAX_CONNECTIONS_PER_CLIENT", "+128"),
            ("GM_PUBLIC_ORIGINS", " https://meter.example, ,https://other.example "),
            ("GM_TRUSTED_PROXIES", " 192.0.2.129/24,2001:db8::1/64, "),
            ("GM_MAX_SESSION_DURATION", " +2h1.5s "),
        ],
        &[],
    )
    .unwrap();
    assert_eq!((c.server_name.as_str(), c.server_location.as_str()), ("meter", "EU"));
    assert!(c.verbose && c.result_history_default);
    assert_eq!((c.max_connections, c.max_connections_per_client), (1024, 128));
    assert_eq!(c.public.both, ["https://meter.example", "https://other.example"]);
    assert_eq!(
        c.trusted_proxies.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["192.0.2.0/24", "2001:db8::/64"]
    );
    assert_eq!(c.max_session_duration, Duration::from_millis(7_201_500));
    let defaults = Config::default();
    let c = load(
        &[
            ("GM_VERBOSE", ""),
            ("GM_MAX_CONNECTIONS", " "),
            ("GM_MAX_OPERATION_DURATION", ""),
            ("GM_ADVERTISED_NATIVE_ENDPOINTS", ""),
        ],
        &["-max-active-sessions=", "-verbose="],
    )
    .unwrap();
    assert!(!c.verbose && c.advertised_native.is_none());
    assert_eq!(c.max_connections, defaults.max_connections);
    assert_eq!(c.limits.sessions, defaults.limits.sessions);
    assert_eq!(c.max_operation_duration, defaults.max_operation_duration);
    for raw in ["none", " , "] {
        let c = load(
            &[
                ("GM_ADVERTISED_NATIVE_ENDPOINTS", raw),
                ("GM_PUBLIC_ORIGINS", "https://meter.example"),
            ],
            &[],
        )
        .unwrap();
        assert!(c.advertised_native.as_ref().unwrap().is_empty(), "{raw:?}");
    }
    let c = load(&[("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1-clear,http1-clear")], &[]).unwrap();
    assert_eq!(
        c.advertised_native.clone().unwrap().into_iter().collect::<Vec<_>>(),
        [NativeKind::H1]
    );
}

#[test]
fn flags_complete_and_override_the_environment() {
    let c = load(
        &[
            ("GM_H2_ADDR", ":7443"),
            ("GM_SERVER_NAME", "environment"),
            ("GM_VERBOSE", "true"),
        ],
        &[
            "-tls-cert",
            "/cert.pem",
            "--tls-key=/key.pem",
            "-name",
            "edge",
            "-verbose=FALSE",
            "-result-history-default",
            "--max-connections-per-client=010",
            "-max-operation-duration",
            "2m",
            "--",
        ],
    )
    .unwrap();
    assert_eq!(c.listener(NativeKind::H2).address, ":7443");
    assert_eq!((c.tls_cert.as_str(), c.server_name.as_str()), ("/cert.pem", "edge"));
    assert!(!c.verbose && c.result_history_default);
    assert_eq!(c.max_connections_per_client, 10);
    assert_eq!(c.max_operation_duration, Duration::from_secs(120));
    let c = load(
        &[],
        &[
            "-advertised-native-endpoints",
            "none",
            "-public-origins",
            "https://a.example, self",
        ],
    )
    .unwrap();
    assert!(c.advertised_native.as_ref().unwrap().is_empty());
    assert_eq!(c.public.both, ["https://a.example", "self"]);
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
    // Go's texts, and its order of checks.
    let auth = "GM_AUTH_MODE=password GM_AUTH_PUBLIC_URL=https://meter.example GM_AUTH_PASSWORD_HASH=h \
                GM_ADVERTISED_NATIVE_ENDPOINTS=none";
    for (env, expected) in [
        (
            "GM_TRUSTED_PROXIES=10.0.0.1".into(),
            r#": "10.0.0.1": netip.ParsePrefix("10.0.0.1"): no '/'"#,
        ),
        (
            "GM_TRUSTED_PROXIES=10.0.0.0/08".into(),
            r#"ParsePrefix("10.0.0.0/08"): bad bits after slash: "08""#,
        ),
        ("GM_TRUSTED_PROXIES=10.0.0.0/33".into(), "): prefix length out of range"),
        (
            "GM_MAX_OPERATION_DURATION=5".into(),
            r#": time: missing unit in duration "5""#,
        ),
        (
            "GM_MAX_OPERATION_DURATION=1h5q".into(),
            r#": time: unknown unit "q" in duration "1h5q""#,
        ),
        (
            "GM_MAX_OPERATION_DURATION=1h.".into(),
            r#": time: invalid duration "1h.""#,
        ),
        (
            "GM_H1_PUBLIC_ORIGIN=http://a.example GM_PUBLIC_ORIGINS=http://a.example GM_PUBLIC_LATENCY_ORIGINS=x"
                .into(),
            "cannot be both native deterministic and public negotiated",
        ),
        (
            format!("{auth} GM_PUBLIC_ORIGINS=https://meter.example/p"),
            "GM_PUBLIC_ORIGINS contains invalid origin",
        ),
        (
            format!("{auth} GM_AUTH_OIDC_CLIENT_SECRET=s GM_AUTH_OIDC_CLIENT_SECRET_FILE=f"),
            "SECRET_FILE are mutually",
        ),
        (
            format!("{auth} GM_AUTH_OIDC_PROVIDER_NAME=a\tb"),
            "64 bytes of UTF-8 without control characters",
        ),
    ] {
        let env: Vec<_> = env.split(' ').map(|pair: &str| pair.split_once('=').unwrap()).collect();
        let message = failure(&env, &[]);
        assert!(message.contains(expected), "{message}");
    }
    let huge = [
        ("GM_MAX_ACTIVE_MEASUREMENTS", "5000000000"),
        ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "5000000000"),
    ];
    load(&huge, &[]).expect("without HTTP/3, Go takes limits past a QUIC stream count");
    assert!(failure(&[("GM_SERVER_CATALOG", ""), ("GM_SERVER_CATALOG_FILE", "")], &[]).contains("only one"));
    assert!(failure(&[("GM_SERVER_CATALOG_FILE", "/nonexistent-catalog.json")], &[]).contains("/nonexistent-catalog"));
    assert!(failure(&[("GM_MAX_CONNECTIONS", "many")], &["-max-connections=9"]).contains("GM_MAX_CONNECTIONS"));
    for (args, expected) in [
        (&["-not-a-flag"][..], "flag provided but not defined: -not-a-flag"),
        (
            &["-verbose=maybe"],
            "invalid boolean value \"maybe\" for -verbose: must be true/false or 1/0",
        ),
        (
            &["-max-connections=0x200"],
            "invalid value \"0x200\" for flag -max-connections: must be an integer",
        ),
        (&["-max-operation-duration=1d"], "for flag -max-operation-duration"),
        (&["-name"], "flag needs an argument: -name"),
        (&["---name=x"], "bad flag syntax"),
        (&["-=x"], "bad flag syntax"),
        (
            &["-max-connections", "-5"],
            "GM_MAX_CONNECTIONS must be greater than zero",
        ),
        (
            &["-max-connections-per-client=010", "-max-connections=9"],
            "must not exceed GM_MAX_CONNECTIONS",
        ),
    ] {
        let message = failure(&[], args);
        assert!(message.contains(expected), "{args:?}: {message}");
    }
}

#[test]
fn flag_parsing_stops_before_the_first_non_flag_as_in_go() {
    for args in [
        &["serve"][..],
        &["-"],
        &["--", "x"],
        &["-verbose", "false"],
        &["serve", "-max-connections=0"],
    ] {
        load(&[], args).unwrap_or_else(|error| panic!("{args:?}: {error}"));
    }
    assert!(load(&[], &["-verbose", "false"]).unwrap().verbose);
    let config = load(&[], &["-location=a", "--", "-location=b"]).unwrap();
    assert_eq!(config.server_location, "a");
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
    let (code, stderr) = start(&env, &[]);
    assert_eq!(code, None, "{stderr}");
    assert!(stderr.contains(" origin=https://meter.example:8443 "), "{stderr}");
}

#[test]
fn executable_reports_usage_and_refuses_invalid_identity_like_go() {
    let help = server().arg("-h").output().unwrap();
    let usage = String::from_utf8(help.stderr).unwrap();
    assert!(help.status.success() && help.stdout.is_empty());
    for expected in [
        "  graphite-meter hash-password    read a password twice on stdin, print its Argon2id hash\n",
        "  -h1-addr address\n    \tclear HTTP/1.1 listen address (env GM_H1_ADDR) (default :7246)\n",
        "(env GM_ADVERTISED_NATIVE_ENDPOINTS) (default all)\n",
        "  -max-operation-duration duration\n    \tmaximum measurement operation duration \
         (env GM_MAX_OPERATION_DURATION) (default 5m0s)\n",
        "  -verbose\n    \tlog per-second download/upload throughput (env GM_VERBOSE)\n",
    ] {
        assert!(usage.contains(expected), "{expected:?} missing from\n{usage}");
    }
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
        // Go says "load matching TLS certificate/key: open ...", which tls.rs does not yet; both name the file.
        (&tls, &[], "/nonexistent-cert.pem"),
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
    let empty = [
        ("GM_VERBOSE", ""),
        ("GM_MAX_CONNECTIONS", ""),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", ""),
    ];
    let (code, stderr) = start(&empty, &[]);
    assert_eq!(code, None, "empty values must keep their defaults: {stderr}");
}
