//! Go-compatible server flags resolved through the shared configuration loader.
use crate::{config::ConfigError, duration::parse_go_duration};
use std::{collections::BTreeMap, ffi::OsString};

#[derive(Clone, Copy)]
enum Kind {
    String,
    Bool,
    Integer,
    Duration,
}
struct Flag {
    name: &'static str,
    env: &'static str,
    kind: Kind,
}
use Kind::{Bool, Duration, Integer, String as Text};

const FLAGS: &[Flag] = &[
    Flag {
        name: "h1-addr",
        env: "GM_H1_ADDR",
        kind: Text,
    },
    Flag {
        name: "h1-tls-addr",
        env: "GM_H1_TLS_ADDR",
        kind: Text,
    },
    Flag {
        name: "h2-addr",
        env: "GM_H2_ADDR",
        kind: Text,
    },
    Flag {
        name: "h3-addr",
        env: "GM_H3_ADDR",
        kind: Text,
    },
    Flag {
        name: "tls-cert",
        env: "GM_TLS_CERT",
        kind: Text,
    },
    Flag {
        name: "tls-key",
        env: "GM_TLS_KEY",
        kind: Text,
    },
    Flag {
        name: "h1-public-origin",
        env: "GM_H1_PUBLIC_ORIGIN",
        kind: Text,
    },
    Flag {
        name: "h1-tls-public-origin",
        env: "GM_H1_TLS_PUBLIC_ORIGIN",
        kind: Text,
    },
    Flag {
        name: "h2-public-origin",
        env: "GM_H2_PUBLIC_ORIGIN",
        kind: Text,
    },
    Flag {
        name: "h3-public-origin",
        env: "GM_H3_PUBLIC_ORIGIN",
        kind: Text,
    },
    Flag {
        name: "advertised-native-endpoints",
        env: "GM_ADVERTISED_NATIVE_ENDPOINTS",
        kind: Text,
    },
    Flag {
        name: "public-origins",
        env: "GM_PUBLIC_ORIGINS",
        kind: Text,
    },
    Flag {
        name: "public-throughput-origins",
        env: "GM_PUBLIC_THROUGHPUT_ORIGINS",
        kind: Text,
    },
    Flag {
        name: "public-latency-origins",
        env: "GM_PUBLIC_LATENCY_ORIGINS",
        kind: Text,
    },
    Flag {
        name: "name",
        env: "GM_SERVER_NAME",
        kind: Text,
    },
    Flag {
        name: "location",
        env: "GM_SERVER_LOCATION",
        kind: Text,
    },
    Flag {
        name: "result-history-default",
        env: "GM_RESULT_HISTORY_DEFAULT",
        kind: Bool,
    },
    Flag {
        name: "verbose",
        env: "GM_VERBOSE",
        kind: Bool,
    },
    Flag {
        name: "max-active-measurements",
        env: "GM_MAX_ACTIVE_MEASUREMENTS",
        kind: Integer,
    },
    Flag {
        name: "max-active-measurements-per-client",
        env: "GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT",
        kind: Integer,
    },
    Flag {
        name: "max-active-sessions",
        env: "GM_MAX_ACTIVE_SESSIONS",
        kind: Integer,
    },
    Flag {
        name: "max-sessions-per-client",
        env: "GM_MAX_SESSIONS_PER_CLIENT",
        kind: Integer,
    },
    Flag {
        name: "max-connections",
        env: "GM_MAX_CONNECTIONS",
        kind: Integer,
    },
    Flag {
        name: "max-connections-per-client",
        env: "GM_MAX_CONNECTIONS_PER_CLIENT",
        kind: Integer,
    },
    Flag {
        name: "max-operation-duration",
        env: "GM_MAX_OPERATION_DURATION",
        kind: Duration,
    },
    Flag {
        name: "max-session-duration",
        env: "GM_MAX_SESSION_DURATION",
        kind: Duration,
    },
    Flag {
        name: "auth-mode",
        env: "GM_AUTH_MODE",
        kind: Text,
    },
    Flag {
        name: "auth-public-url",
        env: "GM_AUTH_PUBLIC_URL",
        kind: Text,
    },
    Flag {
        name: "auth-password-hash-file",
        env: "GM_AUTH_PASSWORD_HASH_FILE",
        kind: Text,
    },
    Flag {
        name: "auth-oidc-issuer",
        env: "GM_AUTH_OIDC_ISSUER",
        kind: Text,
    },
    Flag {
        name: "auth-oidc-client-id",
        env: "GM_AUTH_OIDC_CLIENT_ID",
        kind: Text,
    },
    Flag {
        name: "auth-oidc-client-secret-file",
        env: "GM_AUTH_OIDC_CLIENT_SECRET_FILE",
        kind: Text,
    },
    Flag {
        name: "auth-oidc-allowed-groups",
        env: "GM_AUTH_OIDC_ALLOWED_GROUPS",
        kind: Text,
    },
    Flag {
        name: "auth-oidc-provider-name",
        env: "GM_AUTH_OIDC_PROVIDER_NAME",
        kind: Text,
    },
];

pub enum Arguments {
    Help,
    Overrides(BTreeMap<String, String>),
}

pub fn parse(args: &[OsString]) -> Result<Arguments, ConfigError> {
    let mut overrides = BTreeMap::new();
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        let argument = argument
            .to_str()
            .ok_or("command line arguments must be UTF-8")?;
        if argument == "--" {
            if args.next().is_some() {
                return Err("unexpected positional argument".into());
            }
            break;
        }
        let flag = argument
            .strip_prefix("--")
            .or_else(|| argument.strip_prefix('-'))
            .ok_or("unexpected positional argument")?;
        let (name, supplied) = flag
            .split_once('=')
            .map_or((flag, None), |(name, value)| (name, Some(value)));
        if matches!(name, "h" | "help") && supplied.is_none() {
            return Ok(Arguments::Help);
        }
        let spec = FLAGS
            .iter()
            .find(|flag| flag.name == name)
            .ok_or_else(|| format!("unknown flag: -{name}"))?;
        let value = match (supplied, spec.kind) {
            (Some(value), _) => value,
            (None, Bool) => "true",
            (None, _) => args
                .next()
                .ok_or_else(|| format!("flag -{name} requires a value"))?
                .to_str()
                .ok_or("command line arguments must be UTF-8")?,
        };
        let value = match spec.kind {
            Bool => match value {
                "1" | "t" | "T" | "TRUE" | "true" | "True" => "true".into(),
                "0" | "f" | "F" | "FALSE" | "false" | "False" => "false".into(),
                _ => return Err(format!("flag -{name} requires a boolean").into()),
            },
            Integer => integer(value)
                .ok_or_else(|| format!("flag -{name} requires a signed 64-bit integer"))?
                .to_string(),
            Duration => {
                parse_go_duration(value)
                    .map_err(|_| format!("flag -{name} requires a Go duration"))?;
                value.into()
            }
            Text => value.into(),
        };
        overrides.insert(spec.env.into(), value);
    }
    Ok(Arguments::Overrides(overrides))
}

fn integer(raw: &str) -> Option<i64> {
    let (negative, raw) = raw.strip_prefix('-').map_or_else(
        || (false, raw.strip_prefix('+').unwrap_or(raw)),
        |raw| (true, raw),
    );
    let (radix, digits, prefixed) = if raw.starts_with("0x") || raw.starts_with("0X") {
        (16, &raw[2..], true)
    } else if raw.starts_with("0b") || raw.starts_with("0B") {
        (2, &raw[2..], true)
    } else if raw.starts_with("0o") || raw.starts_with("0O") {
        (8, &raw[2..], true)
    } else if raw.starts_with('0') && raw.len() > 1 {
        (8, raw, false)
    } else {
        (10, raw, false)
    };
    if digits.is_empty()
        || digits.ends_with('_')
        || digits.contains("__")
        || !prefixed && digits.starts_with('_')
        || digits.chars().any(|ch| ch != '_' && !ch.is_digit(radix))
    {
        return None;
    }
    let digits = digits.replace('_', "");
    let value = i128::from_str_radix(&digits, radix).ok()?;
    i64::try_from(if negative { -value } else { value }).ok()
}

pub fn help() -> String {
    let mut help = String::from(
        "Experimental Graphite Meter Rust server\n\nUsage: graphite-meter-server [flags]\n       graphite-meter-server version\n\nFlags override GM_* environment variables.\nUse -flag value or --flag=value; bare boolean flags enable them.\nExperimental listeners: HTTP/1, HTTPS/WSS, HTTP/2, HTTP/3/WebTransport.\nTUI, OIDC, and hash-password are not integrated yet. Browser assets are included by the mise build tasks.\n\nFlags:\n",
    );
    for flag in FLAGS {
        let value = match flag.kind {
            Text => " value",
            Integer => " integer",
            Duration => " duration",
            Bool => "[=true|false]",
        };
        help.push_str(&format!("  -{}{value}\t{}\n", flag.name, flag.env));
    }
    help.push_str("  -h, --help\tShow this help\n  version, --version\tShow engine version\n");
    help
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, NativeKind};
    use std::time::Duration;

    #[test]
    fn flags_complete_and_override_environment_before_validation_and_reject_invalid_input() {
        let env = BTreeMap::from([
            ("GM_H2_ADDR".into(), ":7443".into()),
            ("GM_SERVER_NAME".into(), "environment".into()),
            ("GM_VERBOSE".into(), "true".into()),
        ]);
        let args = [
            "-tls-cert",
            "/cert.pem",
            "--tls-key=/key.pem",
            "-name",
            "edge",
            "-verbose=false",
            "-result-history-default",
            "--max-connections=0x2_00",
            "-max-operation-duration",
            "2m",
        ]
        .map(OsString::from);
        let Arguments::Overrides(overrides) = parse(&args).unwrap() else {
            panic!("unexpected help")
        };
        let config = Config::from_env_with_overrides(&env, &overrides).unwrap();
        assert_eq!(config.server_name, "edge");
        assert_eq!(config.listener(NativeKind::H2).address, ":7443");
        assert_eq!(config.tls_cert, "/cert.pem");
        assert!(!config.verbose && config.result_history_default);
        assert_eq!(config.max_connections, 512);
        assert_eq!(config.max_operation_duration, Duration::from_secs(120));
        assert_eq!(env["GM_SERVER_NAME"], "environment");
        for args in [
            vec!["-not-a-flag"],
            vec!["-verbose=maybe"],
            vec!["-max-connections=12x"],
            vec!["-max-connections=0x+10"],
            vec!["-max-connections=9223372036854775808"],
            vec!["-max-operation-duration="],
            vec!["-name"],
            vec!["positional"],
        ] {
            assert!(parse(&args.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err());
        }
        for args in [["-max-connections", "-5"], ["-auth-public-url", ""]] {
            let Arguments::Overrides(overrides) = parse(&args.map(OsString::from)).unwrap() else {
                panic!("unexpected help")
            };
            assert!(Config::from_env_with_overrides(&BTreeMap::new(), &overrides).is_err());
        }
    }
}
