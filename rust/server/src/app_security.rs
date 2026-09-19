//! Browser policy derived from configured destinations and validated authority.

use crate::{
    config::{AuthMode, Config, ConfigError},
    preflight::Preflight,
};
use graphite_meter_core::origin::{
    browser_connect_source_supported, canonical_origin, target_origin,
};
use http::{HeaderMap, HeaderValue, uri::Authority};
use std::sync::Arc;

pub struct AppSecurity {
    preflight: Preflight,
    configured_sources: Vec<String>,
    authenticated_host: Option<String>,
    script_policy: String,
}

impl AppSecurity {
    pub fn new(config: Arc<Config>, script_hash: Option<&str>) -> Result<Self, ConfigError> {
        config.validate()?;
        let authenticated_host = if config.auth.mode == AuthMode::Off {
            None
        } else {
            Some(
                target_origin(&config.auth.public_url)?
                    .ok_or("missing authentication origin")?
                    .host,
            )
        };
        let mut script_policy = String::from("script-src 'self'");
        if let Some(hash) = script_hash {
            // The hash comes from embedded bytes, but keep this constructor's
            // boundary independent of its caller and CSP quoting conventions.
            use base64::{Engine, engine::general_purpose::STANDARD};
            let decoded = STANDARD.decode(hash)?;
            if decoded.len() != 32 || STANDARD.encode(&decoded) != hash {
                return Err("invalid application script hash".into());
            }
            script_policy.push_str(&format!(" 'sha256-{hash}'"));
        }
        Ok(Self {
            configured_sources: config.server_catalog.connect_sources(),
            preflight: Preflight::new(config)?,
            authenticated_host,
            script_policy,
        })
    }

    pub fn headers(&self, authority: &str) -> Result<HeaderMap, ConfigError> {
        let authority: Authority = authority.parse()?;
        if authority.as_str().contains('@') {
            return Err("request authority must not contain credentials".into());
        }
        let host = self.authenticated_host.as_deref().unwrap_or_else(|| {
            authority
                .host()
                .trim_start_matches('[')
                .trim_end_matches(']')
        });
        let mut sources = self.configured_sources.clone();
        for source in self.preflight.connect_origins(host)? {
            let http_origin = source
                .replacen("wss://", "https://", 1)
                .replacen("ws://", "http://", 1);
            if canonical_origin(&http_origin).is_ok() && browser_connect_source_supported(&source) {
                sources.push(source);
            }
        }
        sources.sort_unstable();
        sources.dedup();
        let mut csp = String::from(
            "frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'; connect-src 'self'",
        );
        for source in sources {
            csp.push(' ');
            csp.push_str(&source);
        }
        csp.push_str("; ");
        csp.push_str(&self.script_policy);
        let mut headers = HeaderMap::new();
        headers.insert("content-security-policy", HeaderValue::from_str(&csp)?);
        headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
        headers.insert(
            "permissions-policy",
            HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
        );
        if self.authenticated_host.is_some() {
            headers.insert(
                "strict-transport-security",
                HeaderValue::from_static("max-age=31536000"),
            );
        }
        Ok(headers)
    }
}
