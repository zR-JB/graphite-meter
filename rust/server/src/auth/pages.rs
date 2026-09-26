//! Native auth templates share the browser client's compile-time CSS and scripts.
use std::{fmt, sync::LazyLock};

use askama::Template;
use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

pub const STYLES: &str = include_str!("../../../../client/src/auth/auth.css");
pub const THEME_SCRIPT: &str = include_str!("../../../../client/src/auth/theme.js");
pub const PENDING_SCRIPT: &str = include_str!("../../../../client/src/auth/pending.js");

#[derive(Template)]
#[template(path = "auth-login.html")]
pub struct LoginPage<'a> {
    pub csrf: &'a str,
    pub provider: &'a str,
    pub challenge: &'a str,
    pub password: bool,
    pub oidc: bool,
    pub oidc_ready: bool,
    pub notice: &'a str,
    pub status: &'a str,
}

#[derive(Template)]
#[template(path = "auth-cli.html")]
pub struct ApprovalPage<'a> {
    pub browser_capacity: bool,
    pub client_limit: usize,
    pub browser_origin: &'a str,
    pub code: &'a str,
    pub csrf: &'a str,
    pub challenge: &'a str,
}

#[derive(Template)]
#[template(path = "auth-cli-done.html")]
pub struct DonePage {
    pub browser: bool,
}

#[derive(Template)]
#[template(path = "auth-continue.html")]
pub struct ContinuePage<'a> {
    pub challenge: &'a str,
    pub opening: bool,
}

impl ContinuePage<'_> {
    pub fn destination(&self) -> String {
        if self.challenge.is_empty() {
            return "/".into();
        }
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("challenge", self.challenge)
            .finish();
        format!("/auth/cli?{query}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidAuthorizationOrigin;

impl fmt::Display for InvalidAuthorizationOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("authorization origin must be a canonical HTTPS origin")
    }
}
impl std::error::Error for InvalidAuthorizationOrigin {}

static CSP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "default-src 'none'; style-src 'sha256-{}'; script-src 'sha256-{}' 'sha256-{}'; connect-src 'self'; img-src data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        asset_hash(STYLES),
        asset_hash(THEME_SCRIPT),
        asset_hash(PENDING_SCRIPT),
    )
});

fn asset_hash(asset: &str) -> String {
    STANDARD.encode(Sha256::digest(asset.as_bytes()))
}

/// Match auth-page hardening headers. The optional discovered OIDC origin only
/// widens form-action; it cannot inject CSP directives or wildcard sources.
pub fn security_headers(
    authorization_origin: Option<&str>,
) -> Result<HeaderMap, InvalidAuthorizationOrigin> {
    let mut csp = CSP.clone();
    if let Some(origin) = authorization_origin.filter(|origin| !origin.is_empty()) {
        if !super::secure_browser_origin(origin)
            || origin.chars().any(|ch| {
                ch.is_whitespace() || matches!(ch, ';' | '\'' | '"' | '*' | '\\' | '<' | '>')
            })
        {
            return Err(InvalidAuthorizationOrigin);
        }
        csp = csp.replace(
            "form-action 'self'",
            &format!("form-action 'self' {origin}"),
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert(
        "content-security-policy",
        HeaderValue::from_str(&csp).map_err(|_| InvalidAuthorizationOrigin)?,
    );
    Ok(headers)
}
