//! One settings table drives the environment, flags and usage, as Go's `config.settings()` does.
use super::{AuthMode, Config, ConfigError, NativeKind};
use graphite_meter_core::duration::parse_go_duration;
use std::{collections::BTreeSet, ffi::OsString, io::Write, time::Duration};

type Field<T> = fn(&mut Config) -> &mut T;

enum Kind {
    Text(Field<String>),
    List(Field<Vec<String>>),
    Number(Field<usize>),
    Duration(Field<Duration>),
    Bool(Field<bool>),
    Parsed {
        parse: fn(&mut Config, &str) -> Result<(), String>,
        default: &'static str,
    },
}
use Kind::{Bool, Duration as Span, List, Number, Parsed, Text};

struct Setting(&'static str, &'static str, &'static str, Kind);

const H1: usize = NativeKind::H1 as usize;
const H1_TLS: usize = NativeKind::H1Tls as usize;
const H2: usize = NativeKind::H2 as usize;
const H3: usize = NativeKind::H3 as usize;

#[rustfmt::skip]
const SETTINGS: &[Setting] = &[
    Setting("GM_H1_ADDR", "h1-addr", "clear HTTP/1.1 listen `address`", Text(|c| &mut c.native[H1].address)),
    Setting("GM_H1_TLS_ADDR", "h1-tls-addr", "HTTPS HTTP/1.1 listen `address`; empty disables it",
        Text(|c| &mut c.native[H1_TLS].address)),
    Setting("GM_H2_ADDR", "h2-addr", "HTTP/2 TLS listen `address`; empty disables it",
        Text(|c| &mut c.native[H2].address)),
    Setting("GM_H3_ADDR", "h3-addr", "HTTP/3 UDP and bootstrap TCP listen `address`; empty disables it",
        Text(|c| &mut c.native[H3].address)),
    Setting("GM_TLS_CERT", "tls-cert", "TLS certificate PEM `path`", Text(|c| &mut c.tls_cert)),
    Setting("GM_TLS_KEY", "tls-key", "TLS private key PEM `path`", Text(|c| &mut c.tls_key)),
    Setting("GM_H1_PUBLIC_ORIGIN", "h1-public-origin", "public `origin` of the native clear HTTP/1.1 listener",
        Text(|c| &mut c.native[H1].public_origin)),
    Setting("GM_H1_TLS_PUBLIC_ORIGIN", "h1-tls-public-origin", "public `origin` of the native HTTPS HTTP/1.1 listener",
        Text(|c| &mut c.native[H1_TLS].public_origin)),
    Setting("GM_H2_PUBLIC_ORIGIN", "h2-public-origin", "public `origin` of the native HTTP/2 listener",
        Text(|c| &mut c.native[H2].public_origin)),
    Setting("GM_H3_PUBLIC_ORIGIN", "h3-public-origin", "public `origin` of the native HTTP/3 listener",
        Text(|c| &mut c.native[H3].public_origin)),
    Setting("GM_ADVERTISED_NATIVE_ENDPOINTS", "advertised-native-endpoints",
        "all, none, or comma-separated native endpoint `names`", Parsed { parse: advertised_native, default: "all" }),
    Setting("GM_PUBLIC_ORIGINS", "public-origins",
        "comma-separated negotiated `origins` providing throughput and latency", List(|c| &mut c.public.both)),
    Setting("GM_PUBLIC_THROUGHPUT_ORIGINS", "public-throughput-origins",
        "comma-separated negotiated throughput `origins`", List(|c| &mut c.public.throughput)),
    Setting("GM_PUBLIC_LATENCY_ORIGINS", "public-latency-origins", "comma-separated WebSocket latency `origins`",
        List(|c| &mut c.public.latency)),
    Setting("GM_SERVER_NAME", "name", "server `name` advertised in /preflight", Text(|c| &mut c.server_name)),
    Setting("GM_SERVER_LOCATION", "location", "server `location` label", Text(|c| &mut c.server_location)),
    Setting("GM_RESULT_HISTORY_DEFAULT", "result-history-default",
        "save completed browser results on this device by default", Bool(|c| &mut c.result_history_default)),
    Setting("GM_VERBOSE", "verbose", "log per-second download/upload throughput", Bool(|c| &mut c.verbose)),
    Setting("GM_MAX_ACTIVE_MEASUREMENTS", "max-active-measurements",
        "maximum `number` of concurrent measurement handlers", Number(|c| &mut c.limits.operations)),
    Setting("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "max-active-measurements-per-client",
        "maximum `number` of concurrent measurement handlers per client",
        Number(|c| &mut c.limits.operations_per_client)),
    Setting("GM_MAX_ACTIVE_SESSIONS", "max-active-sessions",
        "maximum `number` of concurrent WebTransport sessions, a share of the measurement pool",
        Number(|c| &mut c.limits.sessions)),
    Setting("GM_MAX_SESSIONS_PER_CLIENT", "max-sessions-per-client",
        "maximum `number` of concurrent WebTransport sessions per client",
        Number(|c| &mut c.limits.sessions_per_client)),
    Setting("GM_MAX_BUFFER_BYTES", "max-buffer-bytes", "`bytes` of connection buffers shared by QUIC and HTTP/2",
        Number(|c| &mut c.max_buffer_bytes)),
    Setting("GM_MAX_CONNECTIONS", "max-connections", "maximum `number` of concurrent TCP and QUIC connections",
        Number(|c| &mut c.max_connections)),
    Setting("GM_MAX_CONNECTIONS_PER_CLIENT", "max-connections-per-client",
        "maximum `number` of concurrent connections per direct client", Number(|c| &mut c.max_connections_per_client)),
    Setting("GM_MAX_OPERATION_DURATION", "max-operation-duration", "maximum measurement operation `duration`",
        Span(|c| &mut c.max_operation_duration)),
    Setting("GM_MAX_SESSION_DURATION", "max-session-duration", "maximum WebTransport session `duration`",
        Span(|c| &mut c.max_session_duration)),
    Setting("GM_TRUSTED_PROXIES", "", "", Parsed { parse: trusted_proxies, default: "" }),
    Setting("GM_AUTH_MODE", "auth-mode", "authentication `mode`: off, password, oidc, or hybrid",
        Parsed { parse: auth_mode, default: "off" }),
    Setting("GM_AUTH_PUBLIC_URL", "auth-public-url", "canonical HTTPS UI `origin`", Text(|c| &mut c.auth.public_url)),
    Setting("GM_AUTH_PASSWORD_HASH", "", "", Text(|c| &mut c.auth.password_hash)),
    Setting("GM_AUTH_PASSWORD_HASH_FILE", "auth-password-hash-file",
        "`file` containing the operator Argon2id PHC hash", Text(|c| &mut c.auth.password_hash_file)),
    Setting("GM_AUTH_OIDC_ISSUER", "auth-oidc-issuer", "OIDC issuer `URL`", Text(|c| &mut c.auth.oidc_issuer)),
    Setting("GM_AUTH_OIDC_CLIENT_ID", "auth-oidc-client-id", "OIDC client `ID`",
        Text(|c| &mut c.auth.oidc_client_id)),
    Setting("GM_AUTH_OIDC_CLIENT_SECRET", "", "", Text(|c| &mut c.auth.oidc_client_secret)),
    Setting("GM_AUTH_OIDC_CLIENT_SECRET_FILE", "auth-oidc-client-secret-file",
        "`file` containing the OIDC client secret", Text(|c| &mut c.auth.oidc_secret_file)),
    Setting("GM_AUTH_OIDC_ALLOWED_GROUPS", "auth-oidc-allowed-groups",
        "comma-separated case-sensitive OIDC `groups`", List(|c| &mut c.auth.oidc_allowed_groups)),
    Setting("GM_AUTH_OIDC_PROVIDER_NAME", "auth-oidc-provider-name", "OIDC provider `label`",
        Text(|c| &mut c.auth.oidc_provider_name)),
];

/// Go's precedence: flags override the environment, and a flag error precedes an environment error. None is `-h`.
pub fn load(
    env: impl Fn(&str) -> Option<OsString>,
    args: &[OsString],
    usage: &mut dyn Write,
) -> Result<Option<Config>, ConfigError> {
    let mut config = Config::default();
    let from_env = load_env(&mut config, &env);
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        let argument = argument.to_str().ok_or("command line arguments must be UTF-8")?;
        if argument == "--" && args.len() == 0 {
            break;
        }
        let flag = argument.strip_prefix('-').unwrap_or_default();
        let flag = flag.strip_prefix('-').unwrap_or(flag);
        if flag.is_empty() {
            return Err(format!("unexpected positional argument {argument:?}").into());
        }
        if flag.starts_with(['-', '=']) {
            return Err(failed(usage, format!("bad flag syntax: {argument}")));
        }
        let (name, value) = flag
            .split_once('=')
            .map_or((flag, None), |(name, value)| (name, Some(value)));
        let Some(setting) = SETTINGS
            .iter()
            .find(|Setting(_, flag, ..)| !flag.is_empty() && *flag == name)
        else {
            if matches!(name, "h" | "help") {
                write_usage(usage)?;
                return Ok(None);
            }
            return Err(failed(usage, format!("flag provided but not defined: -{name}")));
        };
        let applied = if let Setting(_, _, _, Bool(_)) = setting {
            let value = value.unwrap_or("true");
            apply(&mut config, setting, value)
                .map_err(|error| format!("invalid boolean value {value:?} for -{name}: {error}"))
        } else {
            let value = match value {
                Some(value) => value,
                None => match args.next() {
                    Some(next) => next.to_str().ok_or("command line arguments must be UTF-8")?,
                    None => return Err(failed(usage, format!("flag needs an argument: -{name}"))),
                },
            };
            apply(&mut config, setting, value)
                .map_err(|error| format!("invalid value {value:?} for flag -{name}: {error}"))
        };
        applied.map_err(|message| failed(usage, message))?;
    }
    from_env?;
    config.validate()?;
    Ok(Some(config))
}

fn load_env(config: &mut Config, env: &impl Fn(&str) -> Option<OsString>) -> Result<(), ConfigError> {
    let text = |name: &str| {
        env(name)
            .map(|value| value.into_string().map_err(|_| format!("{name}: must be UTF-8")))
            .transpose()
    };
    for setting in SETTINGS {
        if let Some(value) = text(setting.0)? {
            apply(config, setting, &value).map_err(|error| format!("{}: {error}", setting.0))?;
        }
    }
    config.server_catalog = crate::catalog::load(
        text("GM_SERVER_CATALOG")?.as_deref(),
        text("GM_SERVER_CATALOG_FILE")?.as_deref(),
    )?;
    Ok(())
}

fn apply(config: &mut Config, Setting(env, _, _, kind): &Setting, raw: &str) -> Result<(), String> {
    if env.starts_with("GM_AUTH_") && *env != "GM_AUTH_MODE" {
        config.auth.explicit = true;
    }
    let value = raw.trim();
    match kind {
        Text(field) => *field(config) = value.into(),
        List(field) => *field(config) = split_list(value),
        Parsed { parse, .. } => parse(config, value)?,
        _ if value.is_empty() => {}
        // A negative limit reads as zero, which validation refuses by name.
        Number(field) => {
            *field(config) = usize::try_from(value.parse::<i64>().map_err(|_| "must be an integer")?).unwrap_or(0);
        }
        Span(field) => {
            let nanos = parse_go_duration(value).map_err(|_| format!("time: invalid duration {value:?}"))?;
            *field(config) = Duration::from_nanos(u64::try_from(nanos).unwrap_or(0));
        }
        Bool(field) => {
            *field(config) = match value.to_ascii_lowercase().as_str() {
                "1" | "t" | "true" => true,
                "0" | "f" | "false" => false,
                _ => return Err("must be true/false or 1/0".into()),
            };
        }
    }
    Ok(())
}

fn advertised_native(config: &mut Config, value: &str) -> Result<(), String> {
    config.advertised_native = match value {
        "" => return Ok(()),
        "all" => None,
        "none" => Some(BTreeSet::new()),
        _ => Some(
            split_list(value)
                .iter()
                .map(|name| NativeKind::parse(name).ok_or_else(|| format!("unknown endpoint {name:?}")))
                .collect::<Result<_, _>>()?,
        ),
    };
    Ok(())
}

fn trusted_proxies(config: &mut Config, value: &str) -> Result<(), String> {
    config.trusted_proxies = split_list(value)
        .iter()
        .map(|raw| {
            let prefix = raw
                .parse::<ipnet::IpNet>()
                .map_err(|error| format!("{raw:?}: {error}"))?;
            if prefix.prefix_len() == 0 {
                return Err(format!(
                    "{raw:?} trusts every address; list the proxy's actual CIDR instead"
                ));
            }
            Ok(prefix.trunc())
        })
        .collect::<Result<_, _>>()?;
    Ok(())
}

fn auth_mode(config: &mut Config, value: &str) -> Result<(), String> {
    config.auth.mode = match value {
        "off" => AuthMode::Off,
        "password" => AuthMode::Password,
        "oidc" => AuthMode::Oidc,
        "hybrid" => AuthMode::Hybrid,
        _ => return Err("must be off, password, oidc, or hybrid".into()),
    };
    Ok(())
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn failed(usage: &mut dyn Write, message: String) -> ConfigError {
    let _ = writeln!(usage, "{message}").and_then(|()| write_usage(usage));
    message.into()
}

fn write_usage(usage: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        usage,
        "Usage:\n  graphite-meter [flags]\n  graphite-meter version | --version\n  graphite-meter hash-password    \
         read a password twice on stdin, print its Argon2id hash\n  graphite-meter --legal           \
         print the reviewed project and dependency notices\n\nFlags:"
    )?;
    let mut defaults = Config::default();
    let mut flags: Vec<_> = SETTINGS.iter().filter(|setting| !setting.1.is_empty()).collect();
    flags.sort_by_key(|setting| setting.1);
    for Setting(env, flag, text, kind) in flags {
        let mut parts = text.split('`');
        let (before, name, after) = (
            parts.next().unwrap_or_default(),
            parts.next(),
            parts.next().unwrap_or_default(),
        );
        let default = match kind {
            Text(field) => field(&mut defaults).clone(),
            Number(field) => field(&mut defaults).to_string(),
            Span(field) => go_duration(*field(&mut defaults)),
            Parsed { default, .. } => (*default).into(),
            List(_) | Bool(_) => String::new(),
        };
        write!(usage, "  -{flag}")?;
        if let Some(name) = name {
            write!(usage, " {name}")?;
        }
        write!(usage, "\n    \t{before}{}{after} (env {env})", name.unwrap_or_default())?;
        if !default.is_empty() {
            write!(usage, " (default {default})")?;
        }
        writeln!(usage)?;
    }
    Ok(())
}

pub(crate) fn go_duration(duration: Duration) -> String {
    match duration.as_secs() {
        seconds @ 0..60 => format!("{seconds}s"),
        seconds @ 60..3600 => format!("{}m{}s", seconds / 60, seconds % 60),
        seconds => format!("{}h{}m{}s", seconds / 3600, seconds / 60 % 60, seconds % 60),
    }
}
