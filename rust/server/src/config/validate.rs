use super::{AuthConfig, AuthMode, Config, ConfigError, NativeKind};
use graphite_meter_core::origin::{Origin, key, target_origin};

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.server_catalog.validate()?;
        self.validate_auth()?;
        self.validate_limits()?;
        self.validate_listeners()?;
        self.validate_origins()
    }

    fn validate_limits(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("GM_MAX_ACTIVE_MEASUREMENTS", self.limits.operations),
            (
                "GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT",
                self.limits.operations_per_client,
            ),
            ("GM_MAX_ACTIVE_SESSIONS", self.limits.sessions),
            (
                "GM_MAX_SESSIONS_PER_CLIENT",
                self.limits.sessions_per_client,
            ),
            ("GM_MAX_CONNECTIONS", self.max_connections),
            (
                "GM_MAX_CONNECTIONS_PER_CLIENT",
                self.max_connections_per_client,
            ),
        ] {
            if value == 0 {
                return Err(format!("{name} must be greater than zero").into());
            }
        }
        for (child_name, child, parent_name, parent) in [
            (
                "GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT",
                self.limits.operations_per_client,
                "GM_MAX_ACTIVE_MEASUREMENTS",
                self.limits.operations,
            ),
            (
                "GM_MAX_ACTIVE_SESSIONS",
                self.limits.sessions,
                "GM_MAX_ACTIVE_MEASUREMENTS",
                self.limits.operations,
            ),
            (
                "GM_MAX_SESSIONS_PER_CLIENT",
                self.limits.sessions_per_client,
                "GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT",
                self.limits.operations_per_client,
            ),
            (
                "GM_MAX_SESSIONS_PER_CLIENT",
                self.limits.sessions_per_client,
                "GM_MAX_ACTIVE_SESSIONS",
                self.limits.sessions,
            ),
            (
                "GM_MAX_CONNECTIONS_PER_CLIENT",
                self.max_connections_per_client,
                "GM_MAX_CONNECTIONS",
                self.max_connections,
            ),
        ] {
            if child > parent {
                return Err(format!("{child_name} must not exceed {parent_name}").into());
            }
        }
        if self.max_operation_duration.is_zero() {
            return Err("GM_MAX_OPERATION_DURATION must be greater than zero".into());
        }
        if self.max_session_duration < self.max_operation_duration {
            return Err(
                "GM_MAX_SESSION_DURATION must be at least GM_MAX_OPERATION_DURATION".into(),
            );
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
            return Err(
                "GM_TLS_CERT and GM_TLS_KEY are required when a native TLS listener is enabled"
                    .into(),
            );
        }
        for (i, kind) in NativeKind::ALL.into_iter().enumerate() {
            for other in &NativeKind::ALL[i + 1..] {
                let address = &self.listener(kind).address;
                if !address.is_empty() && *address == self.listener(*other).address {
                    return Err(format!(
                        "{} and {} must differ",
                        kind.address_env(),
                        other.address_env()
                    )
                    .into());
                }
            }
        }
        if let Some(selected) = &self.advertised_native {
            for &kind in selected {
                if self.listener(kind).address.is_empty() {
                    return Err(format!(
                        "GM_ADVERTISED_NATIVE_ENDPOINTS includes disabled endpoint {:?}",
                        kind.name()
                    )
                    .into());
                }
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
            let scheme = if kind == NativeKind::H1 {
                "http"
            } else {
                "https"
            };
            if !absolute(raw).is_some_and(|origin| origin.scheme == scheme) {
                return Err(format!(
                    "{} must be an origin with {scheme} scheme",
                    kind.origin_env()
                )
                .into());
            }
            if self.native_advertised(kind)
                && deterministic
                    .insert(key(raw), kind.protocol())
                    .is_some_and(|protocol| protocol != kind.protocol())
            {
                return Err(format!(
                    "native origin {raw:?} is advertised with multiple deterministic protocols"
                )
                .into());
            }
        }
        for (name, values) in self.public.lists() {
            for raw in values {
                if raw != "self" && absolute(raw).is_none() {
                    return Err(format!("{name} contains invalid origin {raw:?}").into());
                }
            }
        }
        for raw in self.public.both.iter().chain(&self.public.throughput) {
            if deterministic.contains_key(&key(raw)) {
                return Err(format!(
                    "origin {raw:?} cannot be both native deterministic and public negotiated"
                )
                .into());
            }
        }
        if !NativeKind::ALL
            .into_iter()
            .any(|kind| self.native_advertised(kind))
            && self.public.both.is_empty()
            && self.public.throughput.is_empty()
        {
            return Err("configuration advertises no throughput endpoint".into());
        }
        Ok(())
    }

    fn validate_auth(&self) -> Result<(), ConfigError> {
        if self.auth.mode == AuthMode::Off {
            if self.auth.has_settings() {
                return Err("authentication settings require GM_AUTH_MODE to be enabled".into());
            }
            return Ok(());
        }
        let public = self.auth.validate_public_url()?;
        self.auth.validate_password()?;
        self.auth.validate_oidc()?;
        self.validate_authenticated_origins(&public)
    }

    fn validate_authenticated_origins(&self, public: &Origin) -> Result<(), ConfigError> {
        if self.native_advertised(NativeKind::H1) {
            return Err(
                "clear HTTP/1.1 cannot be advertised when authentication is enabled".into(),
            );
        }
        let check_auth_origin = |name: &str, raw: &str| -> Result<(), ConfigError> {
            if raw.is_empty() || raw == "self" {
                return Ok(());
            }
            if !absolute(raw).is_some_and(|origin| {
                origin.scheme == "https" && origin.host.to_lowercase() == public.host.to_lowercase()
            }) {
                return Err(format!(
                    "{name} must use HTTPS and the canonical authentication hostname"
                )
                .into());
            }
            Ok(())
        };
        for kind in &NativeKind::ALL[1..] {
            check_auth_origin(kind.origin_env(), &self.listener(*kind).public_origin)?;
        }
        for (name, values) in self.public.lists() {
            for raw in values {
                check_auth_origin(name, raw)?;
            }
        }
        Ok(())
    }
}

impl AuthConfig {
    fn has_password_source(&self) -> bool {
        !self.password_hash.is_empty() || !self.password_hash_file.is_empty()
    }

    fn has_oidc_settings(&self) -> bool {
        !self.oidc_issuer.is_empty()
            || !self.oidc_client_id.is_empty()
            || !self.oidc_client_secret.is_empty()
            || !self.oidc_secret_file.is_empty()
            || !self.oidc_allowed_groups.is_empty()
    }

    fn has_settings(&self) -> bool {
        self.explicit
            || !self.public_url.is_empty()
            || self.has_password_source()
            || self.has_oidc_settings()
            || self.oidc_provider_name != "Authelia"
    }

    fn validate_public_url(&self) -> Result<Origin, ConfigError> {
        let public = absolute(&self.public_url)
            .filter(|origin| origin.scheme == "https")
            .ok_or("GM_AUTH_PUBLIC_URL must be an HTTPS origin with no path, query, or fragment")?;
        if public.port.as_deref() == Some("443") {
            return Err("GM_AUTH_PUBLIC_URL must omit the default HTTPS port".into());
        }
        Ok(public)
    }

    fn validate_password(&self) -> Result<(), ConfigError> {
        if !self.password_hash.is_empty() && !self.password_hash_file.is_empty() {
            return Err(
                "GM_AUTH_PASSWORD_HASH and GM_AUTH_PASSWORD_HASH_FILE are mutually exclusive"
                    .into(),
            );
        }
        if self.mode.password() != self.has_password_source() {
            let message = if self.mode.password() {
                "password authentication requires exactly one password hash source"
            } else {
                "password hash configured while password authentication is disabled"
            };
            return Err(message.into());
        }
        Ok(())
    }

    fn validate_oidc(&self) -> Result<(), ConfigError> {
        if !self.oidc_client_secret.is_empty() && !self.oidc_secret_file.is_empty() {
            return Err("GM_AUTH_OIDC_CLIENT_SECRET and GM_AUTH_OIDC_CLIENT_SECRET_FILE are mutually exclusive".into());
        }
        let oidc_complete = !self.oidc_issuer.is_empty()
            && !self.oidc_client_id.is_empty()
            && (!self.oidc_client_secret.is_empty() || !self.oidc_secret_file.is_empty())
            && !self.oidc_allowed_groups.is_empty();
        if self.mode.oidc() && !oidc_complete {
            return Err("OIDC authentication requires issuer, client ID, one client secret source, and allowed groups".into());
        }
        if !self.mode.oidc() && self.has_oidc_settings() {
            return Err("OIDC settings configured while OIDC authentication is disabled".into());
        }
        if self.mode.oidc() {
            self.validate_oidc_issuer()?;
            if self.oidc_provider_name.trim().is_empty() {
                return Err("GM_AUTH_OIDC_PROVIDER_NAME must not be empty".into());
            }
        }
        if self.oidc_provider_name.len() > 64
            || self.oidc_provider_name.chars().any(char::is_control)
        {
            return Err(
                "GM_AUTH_OIDC_PROVIDER_NAME must be at most 64 bytes without control characters"
                    .into(),
            );
        }
        Ok(())
    }

    fn validate_oidc_issuer(&self) -> Result<(), ConfigError> {
        let issuer = url::Url::parse(&self.oidc_issuer).ok();
        let authority = self
            .oidc_issuer
            .split_once("://")
            .map(|(_, rest)| rest.split('/').next().unwrap_or_default());
        if self.oidc_issuer.chars().any(char::is_control)
            || authority.is_none_or(|authority| authority.contains('@'))
            || !issuer.is_some_and(|url| {
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
        {
            return Err(
                "GM_AUTH_OIDC_ISSUER must be an HTTPS URL with no credentials, query, or fragment"
                    .into(),
            );
        }
        Ok(())
    }
}

fn absolute(raw: &str) -> Option<Origin> {
    target_origin(raw).ok().flatten()
}
