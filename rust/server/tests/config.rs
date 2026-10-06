use graphite_meter_proto::origin::BaseUrl;
use graphite_meter_server::config::{self, Config, ListenerKind, Loaded, Methods, Secret};
use std::ffi::OsString;

fn load(env: &[(&str, &str)], args: &[&str]) -> (Result<Loaded, String>, String) {
    let mut usage = Vec::new();
    let lookup = |name: &str| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    };
    let loaded = config::load(lookup, args.iter().map(OsString::from), &mut usage);
    (loaded, String::from_utf8(usage).unwrap())
}

fn config(env: &[(&str, &str)], args: &[&str]) -> Config {
    match load(env, args).0 {
        Ok(Loaded::Config(config)) => *config,
        other => panic!("{env:?} {args:?}: {other:?}"),
    }
}

fn error(env: &[(&str, &str)], args: &[&str]) -> String {
    load(env, args).0.expect_err(&format!("{env:?} {args:?} loaded"))
}

const TLS: [(&str, &str); 2] = [("GM_TLS_CERT", "/c.pem"), ("GM_TLS_KEY", "/k.pem")];

fn with_tls<'a>(env: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    TLS.iter().chain(env).copied().collect()
}

#[test]
fn flag_errors_print_go_messages_then_the_usage() {
    for (args, message) in [
        (&["---x"][..], "bad flag syntax: ---x"),
        (&["-nope"], "flag provided but not defined: -nope"),
        (
            &["-max-connections", "x"],
            "invalid value \"x\" for flag -max-connections: must be an integer",
        ),
        (
            &["-advertised-native-endpoints", "h4"],
            "invalid value \"h4\" for flag -advertised-native-endpoints: unknown endpoint \"h4\"",
        ),
    ] {
        let (loaded, usage) = load(&[], args);
        assert_eq!(loaded.unwrap_err(), message);
        assert_eq!(usage, format!("{message}\n{}", include_str!("usage.txt")));
    }
}

#[test]
fn help_lists_every_flag_with_its_variable_as_go_does() {
    for flag in ["-h", "-help", "--help"] {
        let (loaded, usage) = load(&[("GM_MAX_CONNECTIONS", "x")], &[flag]);
        assert!(matches!(loaded, Ok(Loaded::Help)));
        assert_eq!(usage, include_str!("usage.txt"));
    }
}

#[test]
fn secrets_and_proxies_have_no_flag() {
    let env_only = ["trusted-proxies", "auth-password-hash", "auth-oidc-client-secret", "server-catalog"];
    for flag in env_only.into_iter().chain(["server-catalog-file"]) {
        assert_eq!(error(&[], &[&format!("-{flag}"), "x"]), format!("flag provided but not defined: -{flag}"));
        assert!(!include_str!("usage.txt").contains(&format!("-{flag} ")), "{flag}");
    }
}

#[test]
fn unset_lifetimes_cover_the_stage_limit() {
    let lifetimes = |env: &[(&str, &str)]| {
        let lifetimes = config(env, &[]).lifetimes;
        [lifetimes.operation, lifetimes.session].map(|lifetime| lifetime.as_secs())
    };
    assert_eq!(lifetimes(&[("GM_MAX_STAGE_DURATION", "3h")]), [3 * 3600 + 60, 3 * 3600 + 60]);
    assert_eq!(lifetimes(&[("GM_MAX_STAGE_DURATION", "1m")]), [300, 7200]);
    assert_eq!(
        lifetimes(&[("GM_MAX_STAGE_DURATION", "3h"), ("GM_MAX_OPERATION_DURATION", "10m")]),
        [600, 7200]
    );
    assert_eq!(
        lifetimes(&[("GM_MAX_OPERATION_DURATION", "")]),
        [300, 7200],
        "an empty value is set to its default"
    );
    let explicit = [("GM_MAX_STAGE_DURATION", "3h"), ("GM_MAX_SESSION_DURATION", "1h")];
    let message = "GM_MAX_SESSION_DURATION must be at least GM_MAX_OPERATION_DURATION";
    assert_eq!(error(&explicit, &[]), message);
    let zero = "GM_MAX_OPERATION_DURATION must be greater than zero";
    assert_eq!(error(&[("GM_MAX_OPERATION_DURATION", "0")], &[]), zero);
    assert_eq!(error(&[("GM_MAX_OPERATION_DURATION", "-1s")], &[]), zero);
    for stage in ["999ms", "24h1s", "0"] {
        let message = "GM_MAX_STAGE_DURATION must be from 1s to 24h";
        assert_eq!(error(&[("GM_MAX_STAGE_DURATION", stage)], &[]), message, "{stage}");
    }
}

#[test]
fn trusted_proxies_take_go_prefixes_without_default_routes() {
    let config = config(&[("GM_TRUSTED_PROXIES", "192.0.2.77/24, 2001:db8::1/48")], &[]);
    let proxies: Vec<_> = config.trusted_proxies.iter().map(ToString::to_string).collect();
    assert_eq!(proxies, ["192.0.2.0/24", "2001:db8::/48"]);
    for (value, message) in [
        ("0.0.0.0/0", "\"0.0.0.0/0\" trusts every address; list the proxy's actual CIDR instead"),
        ("::/0", "\"::/0\" trusts every address; list the proxy's actual CIDR instead"),
        (
            "192.0.2.0/024",
            "\"192.0.2.0/024\": netip.ParsePrefix(\"192.0.2.0/024\"): bad bits after slash: \"024\"",
        ),
        ("192.0.2.1", "\"192.0.2.1\": netip.ParsePrefix(\"192.0.2.1\"): no '/'"),
    ] {
        assert_eq!(error(&[("GM_TRUSTED_PROXIES", value)], &[]), format!("GM_TRUSTED_PROXIES: {message}"));
    }
}

#[test]
fn listeners_need_distinct_addresses_and_tls_files() {
    let all = with_tls(&[("GM_H1_TLS_ADDR", ":8443"), ("GM_H2_ADDR", ":8444"), ("GM_H3_ADDR", ":8445")]);
    let config = config(&all, &["-advertised-native-endpoints", "http2,http3"]);
    let kinds: Vec<_> = config
        .listeners
        .iter()
        .map(|listener| (listener.kind, listener.advertised))
        .collect();
    let expected = [
        (ListenerKind::H1, false),
        (ListenerKind::H1Tls, false),
        (ListenerKind::H2, true),
        (ListenerKind::H3, true),
    ];
    assert_eq!(kinds, expected);
    assert_eq!(config.tls.unwrap().cert.to_str(), Some("/c.pem"));
    let message = "GM_TLS_CERT and GM_TLS_KEY are required when a native TLS listener is enabled";
    assert_eq!(error(&[("GM_H2_ADDR", ":8444"), ("GM_TLS_CERT", "/c.pem")], &[]), message);
    assert_eq!(error(&with_tls(&[("GM_H3_ADDR", ":7246")]), &[]), "GM_H1_ADDR and GM_H3_ADDR must differ");
    let message = "GM_ADVERTISED_NATIVE_ENDPOINTS includes disabled endpoint \"http3\"";
    assert_eq!(error(&[("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1-clear, http3")], &[]), message);
}

#[test]
fn origins_follow_their_listener_schemes_and_roles() {
    let shared = [("GM_H1_TLS_ADDR", ":8443"), ("GM_H1_TLS_PUBLIC_ORIGIN", "https://m.example")];
    let mut env = with_tls(&shared);
    env.extend([("GM_H2_ADDR", ":8444"), ("GM_H2_PUBLIC_ORIGIN", "https://M.example:443")]);
    let message = "native origin \"https://M.example:443\" is advertised with multiple deterministic protocols";
    assert_eq!(error(&env, &[]), message);
    let mut env = with_tls(&shared);
    env.push(("GM_PUBLIC_THROUGHPUT_ORIGINS", "https://m.example"));
    let message = "origin \"https://m.example\" cannot be both native deterministic and public negotiated";
    assert_eq!(error(&env, &[]), message);
    env.pop();
    env.push(("GM_PUBLIC_LATENCY_ORIGINS", "https://m.example"));
    assert_eq!(config(&env, &[]).public.latency.len(), 1, "a latency-only origin may be native");
    let none = [("GM_ADVERTISED_NATIVE_ENDPOINTS", "none")];
    assert_eq!(error(&none, &[]), "configuration advertises no throughput endpoint");
    let negotiated = [none[0], ("GM_PUBLIC_THROUGHPUT_ORIGINS", "self")];
    assert_eq!(config(&negotiated, &[]).public.throughput, [BaseUrl::Served]);
}

/// A password deployment behind TLS on `meter.example`.
fn password_auth() -> Vec<(&'static str, &'static str)> {
    with_tls(&[
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", "HTTPS://Meter.Example:08443"),
        ("GM_AUTH_PASSWORD_HASH_FILE", "/hash"),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1-tls"),
        ("GM_H1_TLS_ADDR", ":8443"),
    ])
}

#[test]
fn authentication_settings_are_complete_and_canonical() {
    let auth = config(&password_auth(), &[]).auth.unwrap();
    assert_eq!(auth.public_origin.to_string(), "https://meter.example:8443");
    assert!(matches!(auth.methods, Methods::Password(Secret::File(ref path)) if path.to_str() == Some("/hash")));
    let mut env = password_auth();
    env.extend([
        ("GM_AUTH_MODE", "hybrid"),
        ("GM_AUTH_OIDC_ISSUER", "https://id.example/realm"),
        ("GM_AUTH_OIDC_CLIENT_ID", "meter"),
        ("GM_AUTH_OIDC_CLIENT_SECRET", "s3cret"),
        ("GM_AUTH_OIDC_ALLOWED_GROUPS", "ops, admins"),
    ]);
    let env = dedupe(env);
    let auth = config(&env, &[]).auth.unwrap();
    let oidc = auth.methods.oidc().unwrap();
    assert_eq!(
        (oidc.allowed_groups.as_slice(), auth.provider.as_str()),
        (&["ops".into(), "admins".into()][..], "Authelia")
    );
    assert!(auth.methods.password().is_some());
    assert!(!format!("{:?}", auth.methods).contains("s3cret"), "secrets stay out of Debug");
}

/// The environment with later entries replacing earlier ones of the same name.
fn dedupe<'a>(env: Vec<(&'a str, &'a str)>) -> Vec<(&'a str, &'a str)> {
    let mut kept: Vec<(&str, &str)> = Vec::new();
    for (name, value) in env {
        kept.retain(|(other, _)| *other != name);
        kept.push((name, value));
    }
    kept
}

#[test]
fn authentication_refusals_read_as_go_does() {
    let with = |changes: &[(&'static str, &'static str)]| {
        let mut env = password_auth();
        env.extend_from_slice(changes);
        error(&dedupe(env), &[])
    };
    assert_eq!(
        error(&[("GM_AUTH_PUBLIC_URL", "")], &[]),
        "authentication settings require GM_AUTH_MODE to be enabled"
    );
    assert_eq!(with(&[("GM_AUTH_MODE", "ldap")]), "GM_AUTH_MODE must be off, password, oidc, or hybrid");
    for (changes, message) in [
        (
            &[("GM_AUTH_PUBLIC_URL", "https://meter.example/ui")][..],
            "GM_AUTH_PUBLIC_URL must be an HTTPS origin with no path, query, or fragment",
        ),
        (
            &[("GM_AUTH_PUBLIC_URL", "https://meter.example:443")],
            "GM_AUTH_PUBLIC_URL must omit the default HTTPS port",
        ),
        (
            &[("GM_AUTH_PASSWORD_HASH", "$argon2id$")],
            "GM_AUTH_PASSWORD_HASH and GM_AUTH_PASSWORD_HASH_FILE are mutually exclusive",
        ),
        (
            &[("GM_AUTH_MODE", "oidc")],
            "password hash configured while password authentication is disabled",
        ),
        (
            &[("GM_AUTH_OIDC_CLIENT_ID", "meter")],
            "OIDC settings configured while OIDC authentication is disabled",
        ),
        (
            &[("GM_AUTH_MODE", "hybrid")],
            "OIDC authentication requires issuer, client ID, one client secret source, and allowed groups",
        ),
        (
            &[("GM_ADVERTISED_NATIVE_ENDPOINTS", "all")],
            "clear HTTP/1.1 cannot be advertised when authentication is enabled",
        ),
        (
            &[("GM_PUBLIC_ORIGINS", "self,https://other.example")],
            "GM_PUBLIC_ORIGINS must use HTTPS and the canonical authentication hostname",
        ),
    ] {
        assert_eq!(with(changes), message, "{changes:?}");
    }
    let oidc = [
        ("GM_AUTH_MODE", "hybrid"),
        ("GM_AUTH_OIDC_ISSUER", "https://id.example/realm"),
        ("GM_AUTH_OIDC_CLIENT_ID", "meter"),
        ("GM_AUTH_OIDC_CLIENT_SECRET_FILE", "/secret"),
        ("GM_AUTH_OIDC_ALLOWED_GROUPS", "ops"),
    ];
    let issuer = "GM_AUTH_OIDC_ISSUER must be an HTTPS URL with no credentials, query, or fragment";
    let name = "GM_AUTH_OIDC_PROVIDER_NAME must be at most 64 bytes of UTF-8 without control characters";
    let long = "x".repeat(65);
    for (change, message) in [
        (("GM_AUTH_OIDC_ISSUER", "http://id.example"), issuer),
        (("GM_AUTH_OIDC_ISSUER", "https://id.example/?realm=1"), issuer),
        (("GM_AUTH_OIDC_PROVIDER_NAME", long.as_str()), name),
    ] {
        let mut env: Vec<(&str, &str)> = password_auth();
        env.extend(oidc);
        env.push(change);
        assert_eq!(error(&dedupe(env), &[]), message, "{change:?}");
    }
}
