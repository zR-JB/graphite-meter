//! Auth pages, rendered from Go's own templates as its html/template renders them.
use std::{fmt::Write, sync::LazyLock};

use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

pub const STYLES: &str = include_str!("../../../../go/internal/auth/assets/auth.css");
pub const THEME_SCRIPT: &str = include_str!("../../../../go/internal/auth/assets/theme.js");
pub const PENDING_SCRIPT: &str = include_str!("../../../../go/internal/auth/assets/pending.js");
static LOGIN: LazyLock<Parts> = LazyLock::new(|| parts(include_str!("../../../../go/internal/auth/assets/login.tmpl")));
static CLI: LazyLock<Parts> = LazyLock::new(|| parts(include_str!("../../../../go/internal/auth/assets/cli.tmpl")));
static CLI_DONE: LazyLock<Parts> =
    LazyLock::new(|| parts(include_str!("../../../../go/internal/auth/assets/cli-done.tmpl")));
static CONTINUE: LazyLock<Parts> =
    LazyLock::new(|| parts(include_str!("../../../../go/internal/auth/assets/continue.tmpl")));

/// A template's actions, each with the text after it, where the first action is empty.
type Parts = Vec<(String, String)>;

/// `template` without its comments, split at its actions once rather than on every page.
fn parts(template: &str) -> Parts {
    let mut source = template.to_owned();
    while let Some(start) = source.find("<!--") {
        let end = source[start..].find("-->").map_or(source.len(), |end| start + end + 3);
        source.replace_range(start..end, "");
    }
    let mut parts = source.split("{{");
    let first = parts.next().unwrap_or_default();
    let actions = parts.map(|part| part.split_once("}}").expect("closed action"));
    let parts = std::iter::once(("", first)).chain(actions);
    parts.map(|(action, text)| (action.into(), text.into())).collect()
}

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

impl LoginPage<'_> {
    pub fn render(&self) -> String {
        render(
            &LOGIN,
            &[
                ("CSRF", self.csrf),
                ("Provider", self.provider),
                ("Challenge", self.challenge),
                ("Notice", self.notice),
                ("Status", self.status),
                ("Password", flag(self.password)),
                ("OIDC", flag(self.oidc)),
                ("OIDCReady", flag(self.oidc_ready)),
            ],
        )
    }
}

pub fn approval_page(code: &str, csrf: &str, challenge: &str, browser_origin: &str) -> String {
    render(
        &CLI,
        &[
            ("Code", code),
            ("CSRF", csrf),
            ("Challenge", challenge),
            ("BrowserOrigin", browser_origin),
        ],
    )
}

pub fn capacity_page() -> String {
    let limit = super::grant::MAX_SESSION_GRANTS.to_string();
    render(&CLI, &[("BrowserCapacity", "true"), ("ClientLimit", &limit)])
}

pub fn done_page(browser: bool) -> String {
    render(&CLI_DONE, &[("Browser", flag(browser))])
}

pub fn continue_page(challenge: &str, opening: bool) -> String {
    render(&CONTINUE, &[("Challenge", challenge), ("Opening", flag(opening))])
}

fn flag(on: bool) -> &'static str {
    if on { "true" } else { "" }
}

/// Go's html/template for what these templates use: fields; `template`; and `if`, `else if` and `else` on a
/// field, on `not` or `and` of fields, or on `eq` with a string. A field is escaped for HTML, or for a URL query
/// within an href.
fn render(template: &Parts, fields: &[(&str, &str)]) -> String {
    let field = |name: &str| {
        let name = name.trim_start_matches('.');
        fields
            .iter()
            .find(|(key, _)| *key == name)
            .map_or("", |(_, value)| value)
    };
    let test = |condition: &str| match condition.split(' ').collect::<Vec<_>>()[..] {
        ["not", name] => field(name).is_empty(),
        ["and", first, second] => !field(first).is_empty() && !field(second).is_empty(),
        ["eq", name, text] => field(name) == text.trim_matches('"'),
        [name] => !field(name).is_empty(),
        _ => unreachable!("template condition {condition}"),
    };
    let (mut out, mut branches) = (String::new(), Vec::<(bool, bool)>::new());
    for (action, text) in template {
        let shown = |branches: &[(bool, bool)]| branches.iter().all(|&(_, on)| on);
        if let Some(condition) = action.strip_prefix("if ") {
            branches.push((test(condition), test(condition)));
        } else if let Some((taken, on)) = branches.last_mut().filter(|_| action.starts_with("else")) {
            *on = !*taken && action.strip_prefix("else if ").is_none_or(test);
            *taken |= *on;
        } else if action == "end" {
            branches.pop();
        } else if shown(&branches) {
            match action.as_str() {
                "" => {}
                "template \"theme\"" => write!(out, "<script>{THEME_SCRIPT}</script>").expect("string writer"),
                "template \"pending\"" => write!(out, "<script>{PENDING_SCRIPT}</script>").expect("string writer"),
                ".Styles" => out.push_str(STYLES),
                name if out.rfind("href=\"").is_some_and(|at| !out[at + 6..].contains('"')) => {
                    for byte in field(name).bytes() {
                        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                            out.push(byte.into());
                        } else {
                            write!(out, "%{byte:02x}").expect("string writer");
                        }
                    }
                }
                name => out.push_str(&escape(field(name))),
            }
        }
        if shown(&branches) {
            out.push_str(text);
        }
    }
    out
}

/// Go's html/template escaping of text and quoted attribute values.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\0' => escaped.push('\u{fffd}'),
            '"' => escaped.push_str("&#34;"),
            '&' => escaped.push_str("&amp;"),
            '\'' => escaped.push_str("&#39;"),
            '+' => escaped.push_str("&#43;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            character => escaped.push(character),
        }
    }
    escaped
}

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

/// Auth-page hardening headers. A discovered authorization origin joins form-action, only as a canonical HTTPS
/// origin, which can name no other directive or source.
pub fn security_headers(authorization_origin: Option<&str>) -> Option<HeaderMap> {
    let mut csp = CSP.clone();
    if let Some(origin) = authorization_origin.filter(|origin| !origin.is_empty()) {
        if !super::secure_browser_origin(origin) {
            return None;
        }
        csp = csp.replace("form-action 'self'", &format!("form-action 'self' {origin}"));
    }
    let mut headers = HeaderMap::new();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    harden(&mut headers, false);
    headers.insert("content-security-policy", HeaderValue::from_str(&csp).ok()?);
    Some(headers)
}

/// Go's hardening headers, and HSTS for this host alone once a request under authentication is known secure.
pub fn harden(headers: &mut HeaderMap, secure: bool) {
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if secure {
        headers.insert(
            http::header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
}
