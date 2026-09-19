use std::{collections::BTreeMap, time::Duration};

use super::{AuthMode, Config, ConfigError, NativeKind};
use crate::duration::parse_go_duration;

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with_overrides(&BTreeMap::new())
    }

    /// Snapshot the environment, then apply command-line values before the
    /// shared parser validates the complete configuration.
    pub fn load_with_overrides(overrides: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let mut env = BTreeMap::new();
        for (key, value) in std::env::vars_os() {
            if let Some(key) = key.to_str().filter(|key| key.starts_with("GM_")) {
                let value = value
                    .into_string()
                    .map_err(|_| format!("{key} is not UTF-8"))?;
                env.insert(key.to_owned(), value);
            }
        }
        Self::from_env_with_overrides(&env, overrides)
    }

    pub fn from_env_with_overrides(
        env: &BTreeMap<String, String>,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Self, ConfigError> {
        let mut merged = env.clone();
        merged.extend(overrides.clone());
        Self::from_env(&merged)
    }

    pub fn from_env(env: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        for kind in NativeKind::ALL {
            let listener = &mut config.native[kind as usize];
            assign(env, kind.address_env(), &mut listener.address);
            assign(env, kind.origin_env(), &mut listener.public_origin);
        }
        for (name, dst) in [
            ("GM_TLS_CERT", &mut config.tls_cert),
            ("GM_TLS_KEY", &mut config.tls_key),
            ("GM_SERVER_NAME", &mut config.server_name),
            ("GM_SERVER_LOCATION", &mut config.server_location),
        ] {
            assign(env, name, dst);
        }
        for (name, dst) in [
            ("GM_AUTH_PUBLIC_URL", &mut config.auth.public_url),
            ("GM_AUTH_PASSWORD_HASH", &mut config.auth.password_hash),
            (
                "GM_AUTH_PASSWORD_HASH_FILE",
                &mut config.auth.password_hash_file,
            ),
            ("GM_AUTH_OIDC_ISSUER", &mut config.auth.oidc_issuer),
            ("GM_AUTH_OIDC_CLIENT_ID", &mut config.auth.oidc_client_id),
            (
                "GM_AUTH_OIDC_CLIENT_SECRET",
                &mut config.auth.oidc_client_secret,
            ),
            (
                "GM_AUTH_OIDC_CLIENT_SECRET_FILE",
                &mut config.auth.oidc_secret_file,
            ),
            (
                "GM_AUTH_OIDC_PROVIDER_NAME",
                &mut config.auth.oidc_provider_name,
            ),
        ] {
            if env.contains_key(name) {
                config.auth.explicit = true;
            }
            assign(env, name, dst);
        }
        if let Some(mode) = env.get("GM_AUTH_MODE") {
            config.auth.mode = match mode.trim() {
                "off" => AuthMode::Off,
                "password" => AuthMode::Password,
                "oidc" => AuthMode::Oidc,
                "hybrid" => AuthMode::Hybrid,
                _ => return Err("GM_AUTH_MODE must be off, password, oidc, or hybrid".into()),
            };
        }
        if let Some(value) = env.get("GM_AUTH_OIDC_ALLOWED_GROUPS") {
            config.auth.explicit = true;
            config.auth.oidc_allowed_groups = split_list(value);
        }
        if let Some(raw) = env.get("GM_ADVERTISED_NATIVE_ENDPOINTS") {
            config.advertised_native = match raw.trim() {
                "all" => None,
                "" | "none" => Some(Default::default()),
                _ => Some(
                    split_list(raw)
                        .into_iter()
                        .map(|name| {
                            NativeKind::parse(&name).ok_or_else(|| {
                                format!("GM_ADVERTISED_NATIVE_ENDPOINTS: unknown endpoint {name:?}")
                            })
                        })
                        .collect::<Result<_, _>>()?,
                ),
            };
        }
        for (name, dst) in [
            ("GM_PUBLIC_ORIGINS", &mut config.public.both),
            (
                "GM_PUBLIC_THROUGHPUT_ORIGINS",
                &mut config.public.throughput,
            ),
            ("GM_PUBLIC_LATENCY_ORIGINS", &mut config.public.latency),
        ] {
            if let Some(raw) = env.get(name) {
                *dst = split_list(raw);
            }
        }
        config.server_catalog = crate::catalog::load(
            env.get("GM_SERVER_CATALOG").map(String::as_str),
            env.get("GM_SERVER_CATALOG_FILE").map(String::as_str),
        )?;
        for (name, dst) in [
            ("GM_VERBOSE", &mut config.verbose),
            (
                "GM_RESULT_HISTORY_DEFAULT",
                &mut config.result_history_default,
            ),
        ] {
            if let Some(raw) = env.get(name) {
                *dst = match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" => true,
                    "0" | "false" => false,
                    _ => return Err(format!("{name} must be true/false or 1/0").into()),
                };
            }
        }
        for (name, dst) in [
            ("GM_MAX_ACTIVE_MEASUREMENTS", &mut config.limits.operations),
            (
                "GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT",
                &mut config.limits.operations_per_client,
            ),
            ("GM_MAX_ACTIVE_SESSIONS", &mut config.limits.sessions),
            (
                "GM_MAX_SESSIONS_PER_CLIENT",
                &mut config.limits.sessions_per_client,
            ),
            ("GM_MAX_CONNECTIONS", &mut config.max_connections),
            (
                "GM_MAX_CONNECTIONS_PER_CLIENT",
                &mut config.max_connections_per_client,
            ),
        ] {
            if let Some(raw) = env.get(name) {
                let value = raw
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| format!("{name} must be an integer"))?;
                *dst = usize::try_from(value)
                    .map_err(|_| format!("{name} must be greater than zero"))?;
            }
        }
        for (name, dst) in [
            (
                "GM_MAX_OPERATION_DURATION",
                &mut config.max_operation_duration,
            ),
            ("GM_MAX_SESSION_DURATION", &mut config.max_session_duration),
        ] {
            if let Some(raw) = env.get(name).filter(|value| !value.is_empty()) {
                let nanos = parse_go_duration(raw).map_err(|error| format!("{name}: {error}"))?;
                let nanos =
                    u64::try_from(nanos).map_err(|_| format!("{name} must not be negative"))?;
                *dst = Duration::from_nanos(nanos);
            }
        }
        if let Some(raw) = env
            .get("GM_TRUSTED_PROXIES")
            .filter(|value| !value.is_empty())
        {
            for value in raw.split(',') {
                let prefix = value
                    .trim()
                    .parse::<ipnet::IpNet>()
                    .map_err(|error| format!("GM_TRUSTED_PROXIES: {value:?}: {error}"))?;
                if prefix.prefix_len() == 0 {
                    return Err("GM_TRUSTED_PROXIES must not trust every address".into());
                }
                config.trusted_proxies.push(prefix.trunc());
            }
        }
        config.validate()?;
        Ok(config)
    }
}

fn assign(env: &BTreeMap<String, String>, name: &str, dst: &mut String) {
    if let Some(value) = env.get(name) {
        *dst = value.trim().to_owned();
    }
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}
