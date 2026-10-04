use super::{AuthConfig, AuthMode, Config, ConfigError, NativeKind};
use graphite_meter_core::origin::{Origin, key, split_url, target_origin};

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.published_catalog()
            .validate()
            .map_err(|error| format!("GM_SERVER_NAME, GM_SERVER_LOCATION or the server catalogue: {error}"))?;
        self.validate_auth()?;
        self.validate_limits()?;
        self.validate_listeners()?;
        self.validate_origins()?;
        // Go has no buffer budget, so its checks come first.
        crate::budget::check_configured(self)
    }

    fn validate_limits(&self) -> Result<(), ConfigError> {
        let operations = ("GM_MAX_ACTIVE_MEASUREMENTS", self.limits.operations);
        let client_operations = ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", self.limits.operations_per_client);
        let sessions = ("GM_MAX_ACTIVE_SESSIONS", self.limits.sessions);
        let client_sessions = ("GM_MAX_SESSIONS_PER_CLIENT", self.limits.sessions_per_client);
        let connections = ("GM_MAX_CONNECTIONS", self.max_connections);
        let client_connections = ("GM_MAX_CONNECTIONS_PER_CLIENT", self.max_connections_per_client);
        let limits = [operations, client_operations, sessions, client_sessions, connections, client_connections];
        if let Some((name, _)) = limits.into_iter().find(|&(_, value)| value == 0) {
            return Err(format!("{name} must be greater than zero").into());
        }
        // Sessions are a share of the pool, and no client may take the whole session budget.
        for ((name, value), (parent, limit)) in [
            (client_operations, operations),
            (sessions, operations),
            (client_sessions, client_operations),
            (client_sessions, sessions),
            (client_connections, connections),
        ] {
            if value > limit {
                return Err(format!("{name} must not exceed {parent}").into());
            }
        }
        if self.max_operation_duration.is_zero() {
            return Err("GM_MAX_OPERATION_DURATION must be greater than zero".into());
        }
        if self.max_session_duration < self.max_operation_duration {
            return Err("GM_MAX_SESSION_DURATION must be at least GM_MAX_OPERATION_DURATION".into());
        }
        if !(graphite_meter_core::discovery::MIN_STAGE_LIMIT..=graphite_meter_core::discovery::MAX_STAGE_LIMIT)
            .contains(&self.max_stage_duration)
        {
            return Err("GM_MAX_STAGE_DURATION must be from 1s to 24h".into());
        }
        Ok(())
    }

    fn validate_listeners(&self) -> Result<(), ConfigError> {
        if self.listener(NativeKind::H1).address.is_empty() {
            return Err("GM_H1_ADDR must not be empty".into());
        }
        let tls_listener_enabled = NativeKind::ALL[1..]
            .iter()
            .any(|&kind| !self.listener(kind).address.is_empty());
        if tls_listener_enabled && (self.tls_cert.is_empty() || self.tls_key.is_empty()) {
            return Err("GM_TLS_CERT and GM_TLS_KEY are required when a native TLS listener is enabled".into());
        }
        for (i, kind) in NativeKind::ALL.into_iter().enumerate() {
            for other in &NativeKind::ALL[i + 1..] {
                let address = &self.listener(kind).address;
                if !address.is_empty() && *address == self.listener(*other).address {
                    return Err(format!("{} and {} must differ", kind.address_env(), other.address_env()).into());
                }
            }
        }
        for &kind in self.advertised_native.iter().flatten() {
            if self.listener(kind).address.is_empty() {
                let name = kind.name();
                return Err(format!("GM_ADVERTISED_NATIVE_ENDPOINTS includes disabled endpoint {name:?}").into());
            }
        }
        Ok(())
    }

    fn validate_origins(&self) -> Result<(), ConfigError> {
        let mut deterministic = std::collections::BTreeMap::new();
        for kind in NativeKind::ALL {
            let raw = &self.listener(kind).public_origin;
            if raw.is_empty() {
                continue;
            }
            let scheme = if kind == NativeKind::H1 { "http" } else { "https" };
            if !absolute(raw).is_some_and(|origin| origin.scheme == scheme) {
                return Err(format!("{} must be an origin with {scheme} scheme", kind.origin_env()).into());
            }
            if self.native_advertised(kind)
                && deterministic
                    .insert(key(raw), kind.protocol())
                    .is_some_and(|protocol| protocol != kind.protocol())
            {
                return Err(format!("native origin {raw:?} is advertised with multiple deterministic protocols").into());
            }
        }
        for (name, values) in self.public.lists() {
            for raw in values {
                if raw != "self" && absolute(raw).is_none() {
                    return Err(format!("{name} contains invalid origin {raw:?}").into());
                }
                if name != "GM_PUBLIC_LATENCY_ORIGINS" && deterministic.contains_key(&key(raw)) {
                    return Err(
                        format!("origin {raw:?} cannot be both native deterministic and public negotiated").into()
                    );
                }
            }
        }
        if !NativeKind::ALL.into_iter().any(|kind| self.native_advertised(kind))
            && self.public.both.is_empty()
            && self.public.throughput.is_empty()
        {
            return Err("configuration advertises no throughput endpoint".into());
        }
        Ok(())
    }

    fn validate_auth(&self) -> Result<(), ConfigError> {
        if self.auth.unknown_mode {
            return Err("GM_AUTH_MODE must be off, password, oidc, or hybrid".into());
        }
        if self.auth.mode == AuthMode::Off {
            if self.auth.explicit {
                return Err("authentication settings require GM_AUTH_MODE to be enabled".into());
            }
            return Ok(());
        }
        let public = self.auth.validate_public_url()?;
        self.auth.validate_secrets()?;
        if self.native_advertised(NativeKind::H1) {
            return Err("clear HTTP/1.1 cannot be advertised when authentication is enabled".into());
        }
        let natives = NativeKind::ALL[1..]
            .iter()
            .map(|&kind| (kind.origin_env(), std::slice::from_ref(&self.listener(kind).public_origin)));
        for (name, values) in self.public.lists().into_iter().chain(natives) {
            for raw in values.iter().filter(|raw| !raw.is_empty() && *raw != "self") {
                // Read as Go's url.Parse reads it; whether it is an origin is checked with the others.
                if !url_host(raw).is_some_and(|(scheme, host)| {
                    scheme.eq_ignore_ascii_case("https") && host.eq_ignore_ascii_case(&public.host)
                }) {
                    return Err(format!("{name} must use HTTPS and the canonical authentication hostname").into());
                }
            }
        }
        Ok(())
    }
}

impl AuthConfig {
    pub(super) fn validate_public_url(&self) -> Result<Origin, ConfigError> {
        let public = absolute(&self.public_url)
            .filter(|origin| origin.scheme == "https")
            .ok_or("GM_AUTH_PUBLIC_URL must be an HTTPS origin with no path, query, or fragment")?;
        if public.port.as_deref() == Some("443") {
            return Err("GM_AUTH_PUBLIC_URL must omit the default HTTPS port".into());
        }
        Ok(public)
    }

    /// Go's validateSecrets, in its order: both exclusions, each method's settings, then the provider's.
    fn validate_secrets(&self) -> Result<(), ConfigError> {
        let (password, oidc) = (self.mode.password(), self.mode.oidc());
        let password_source = !self.password_hash.is_empty() || !self.password_hash_file.is_empty();
        let secret_source = !self.oidc_client_secret.is_empty() || !self.oidc_secret_file.is_empty();
        let (issuer, client, groups) = (&self.oidc_issuer, &self.oidc_client_id, &self.oidc_allowed_groups);
        let oidc_complete = !issuer.is_empty() && !client.is_empty() && secret_source && !groups.is_empty();
        let oidc_settings = !issuer.is_empty() || !client.is_empty() || secret_source || !groups.is_empty();
        let https_issuer =
            split_url(issuer).is_ok_and(|(origin, rest)| origin.scheme == "https" && !rest.contains('?'));
        let name = &self.oidc_provider_name;
        Err(if !self.password_hash.is_empty() && !self.password_hash_file.is_empty() {
            "GM_AUTH_PASSWORD_HASH and GM_AUTH_PASSWORD_HASH_FILE are mutually exclusive"
        } else if !self.oidc_client_secret.is_empty() && !self.oidc_secret_file.is_empty() {
            "GM_AUTH_OIDC_CLIENT_SECRET and GM_AUTH_OIDC_CLIENT_SECRET_FILE are mutually exclusive"
        } else if password && !password_source {
            "password authentication requires exactly one password hash source"
        } else if !password && password_source {
            "password hash configured while password authentication is disabled"
        } else if oidc && !oidc_complete {
            "OIDC authentication requires issuer, client ID, one client secret source, and allowed groups"
        } else if !oidc && oidc_settings {
            "OIDC settings configured while OIDC authentication is disabled"
        } else if oidc && !https_issuer {
            "GM_AUTH_OIDC_ISSUER must be an HTTPS URL with no credentials, query, or fragment"
        } else if oidc && name.trim().is_empty() {
            "GM_AUTH_OIDC_PROVIDER_NAME must not be empty"
        } else if name.len() > 64 || !name.chars().all(graphite_meter_core::text::display_character) {
            "GM_AUTH_OIDC_PROVIDER_NAME must be at most 64 bytes of UTF-8 without control characters"
        } else {
            return Ok(());
        }
        .into())
    }
}

/// The scheme and hostname Go's url.Parse reads from `raw`, where it parses: a port must be numeric.
fn url_host(raw: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = raw.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let (host, port) = match host.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?,
        None => host.rsplit_once(':').unwrap_or((host, "")),
    };
    let port = port.strip_prefix(':').unwrap_or(port);
    port.bytes().all(|byte| byte.is_ascii_digit()).then_some((scheme, host))
}

/// A configured origin, which Go's `CanonicalOrigin` accepts: never on port 0.
fn absolute(raw: &str) -> Option<Origin> {
    target_origin(raw)
        .ok()
        .flatten()
        .filter(|origin| origin.port_number() != 0)
}
