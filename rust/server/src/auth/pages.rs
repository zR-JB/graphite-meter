//! Auth pages render byte for byte as Go's html/template renders its `.tmpl` files, with the same assets.
use std::{
    fmt::{self, Write},
    sync::LazyLock,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

pub const STYLES: &str = include_str!("../../../../go/internal/auth/assets/auth.css");
pub const THEME_SCRIPT: &str = include_str!("../../../../go/internal/auth/assets/theme.js");
pub const PENDING_SCRIPT: &str = include_str!("../../../../go/internal/auth/assets/pending.js");

const ICON: &str = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCI+CiAgPCEtLSBHcmFwaGl0ZSBNZXRlciwgImxhdHRpY2UgbmVlZGxlIjogdGhlIGhleGFnb24gaXMgZ3JhcGhpdGUncyBjYXJib24KICAgICAgIGxhdHRpY2UgYW5kIGEgcGVuY2lsJ3MgY3Jvc3Mtc2VjdGlvbjsgdGhlIG5lZWRsZSBtYWtlcyBpdCBhIG1ldGVyLgogICAgICAgQ29sb3JzIGFkYXB0IHRvIHRoZSBicm93c2VyIGNocm9tZSB2aWEgcHJlZmVycy1jb2xvci1zY2hlbWU7IHRoZQogICAgICAgbGlnaHQtc2NoZW1lIHZhbHVlcyBhcmUgdGhlIGRlZmF1bHRzLiBLZWVwIGluIHN5bmMgd2l0aCB0aGUgYnJhbmQgYW5kCiAgICAgICB0ZXh0IHRva2VucyBpbiBzcmMvYXBwLmNzcyBhbmQgdGhlIGdseXBoIGluIHRoZSBhcHAgdG9wYmFyLiAtLT4KICA8c3R5bGU+CiAgICAuaGV4IHsgc3Ryb2tlOiAjMmY3MTdhOyB9CiAgICAubmVlZGxlIHsgc3Ryb2tlOiAjMjYyNzJhOyB9CiAgICAuaHViIHsgZmlsbDogIzI2MjcyYTsgfQogICAgQG1lZGlhIChwcmVmZXJzLWNvbG9yLXNjaGVtZTogZGFyaykgewogICAgICAuaGV4IHsgc3Ryb2tlOiAjNmRiMGI4OyB9CiAgICAgIC5uZWVkbGUgeyBzdHJva2U6ICNkOWRjZTA7IH0KICAgICAgLmh1YiB7IGZpbGw6ICNkOWRjZTA7IH0KICAgIH0KICA8L3N0eWxlPgogIDxwYXRoIGNsYXNzPSJoZXgiIGQ9Ik0xMiAyLjYgMy45IDcuM3Y5LjRsOC4xIDQuNyA4LjEtNC43VjcuM1oiIGZpbGw9Im5vbmUiIHN0cm9rZS13aWR0aD0iMi40IiBzdHJva2UtbGluZWpvaW49InJvdW5kIi8+CiAgPHBhdGggY2xhc3M9Im5lZWRsZSIgZD0iTTEyIDEyIDE4LjYgOC4yIiBzdHJva2Utd2lkdGg9IjIuNiIgc3Ryb2tlLWxpbmVjYXA9InJvdW5kIi8+CiAgPGNpcmNsZSBjbGFzcz0iaHViIiBjeD0iMTIiIGN5PSIxMiIgcj0iMi40Ii8+Cjwvc3ZnPgo=";
const HEX: &str = r#"<path class="hex" d="M12 2.6 3.9 7.3v9.4l8.1 4.7 8.1-4.7V7.3Z" fill="none" stroke-width="2"/>"#;
const CHECK: &str = r#"<path d="m8.2 12.3 2.4 2.4 5.3-5.4" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>"#;

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
        let (csrf, challenge, provider) = (escape(self.csrf), escape(self.challenge), escape(self.provider));
        let status = |status: &str, text: &'static str| if self.status == status { text } else { "" };
        let notice = match self.notice {
            "" => String::new(),
            "password" => r#"<p class="message" role="alert">Incorrect password. Check it and try again.</p>"#.into(),
            "throttled" => r#"<p class="warn" role="alert">Too many attempts. Wait a minute and try again.</p>"#.into(),
            "provider" => format!(
                r#"<p class="notice" role="alert">{provider} is unreachable right now.{}</p>"#,
                if self.password {
                    " Sign in with the operator password."
                } else {
                    ""
                }
            ),
            "busy" => r#"<p class="notice" role="alert">The server is busy. Try again in a moment.</p>"#.into(),
            "stale" => r#"<p class="notice" role="alert">This sign-in form expired. Reload and try again.</p>"#.into(),
            _ => r#"<p class="message" role="alert">Sign-in failed. Please try again.</p>"#.into(),
        };
        let notice = if notice.is_empty() {
            notice
        } else {
            format!("\n        {notice}\n      ")
        };
        let oidc = if self.oidc {
            let (disabled, unavailable) = if self.oidc_ready {
                ("", String::new())
            } else {
                (
                    "disabled",
                    format!(r#"<p class="notice">{provider} is temporarily unavailable.</p>"#),
                )
            };
            format!(
                r#"
        <form method="post" action="/auth/oidc/start">
          <input type="hidden" name="csrf" value="{csrf}">
          <input type="hidden" name="challenge" value="{challenge}">
          <button type="submit" {disabled}>Continue with {provider}</button>
        </form>
        {unavailable}
      "#
            )
        } else {
            String::new()
        };
        let password = if self.password {
            format!(
                r#"
        <form method="post" action="/auth/password">
          <input type="hidden" name="csrf" value="{csrf}">
          <input type="hidden" name="challenge" value="{challenge}">
          {stripped_comment}
          <label class="offscreen" for="username">Account</label>
          <input class="offscreen" id="username" name="username" type="text" value="operator" autocomplete="username" readonly>
          <label for="password">Operator password</label>
          <input id="password" name="password" type="password" maxlength="1024" autocomplete="current-password" autofocus required>
          <button type="submit">Sign in with operator password</button>
        </form>
      "#,
                stripped_comment = ""
            )
        } else {
            String::new()
        };
        let separator = if self.oidc && self.password {
            r#"<div class="separator">or</div>"#
        } else {
            ""
        };
        page(
            "",
            "Sign in",
            "card",
            &format!(
                r#"      <svg class="mark" viewBox="0 0 24 24" aria-hidden="true">{HEX}<path d="M12 12 18.6 8.2" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/><circle cx="12" cy="12" r="2.1" fill="currentColor"/></svg>
      <h1>Private Graphite Meter</h1>
      <p>Sign in to use this measurement server.</p>
      {}
      {}
      {}
      {notice}
      {oidc}
      {separator}
      {password}
"#,
                status("signed_out", r#"<p class="ok" role="status">You're signed out.</p>"#),
                status(
                    "expired",
                    r#"<p class="notice" role="status">Your session ended. Sign in again.</p>"#
                ),
                status(
                    "renew",
                    r#"<p class="notice" role="status">Sign in again before starting this long test.</p>"#
                ),
            ),
            true,
        )
    }
}

pub fn approval_page(code: &str, csrf: &str, challenge: &str, browser_origin: &str) -> String {
    let (intro, action): (String, _) = if browser_origin.is_empty() {
        (
            r#"
 <h1>Approve terminal client</h1>
      <p>Only approve a client you started yourself. This grants a terminal
        access to run measurements as you until you sign out.</p>
      <p>Confirm that this code matches the code in your terminal.</p>
      "#
            .into(),
            "/auth/cli/approve",
        )
    } else {
        (
            format!(
                r#"
 <h1>Approve browser client</h1>
 <p>Allow this site to run measurements using your sign-in?</p>
 <p><strong>{}</strong></p>
 <p>Confirm that this code matches the code in the requesting Graphite Meter interface.</p>
 "#,
                escape(browser_origin)
            ),
            "/auth/browser/approve",
        )
    };
    let (code, csrf, challenge) = (escape(code), escape(csrf), escape(challenge));
    approval_card(&format!(
        r#"
      {intro}
 <code class="code">{code}</code>
      <form method="post" action="{action}">
        <input type="hidden" name="csrf" value="{csrf}">
        <input type="hidden" name="challenge" value="{challenge}">
        <button type="submit">Approve this client</button>
      </form>
      <p class="notice">If you did not request access for this client, close
        this page and do not approve.</p>
      "#
    ))
}

pub fn capacity_page() -> String {
    approval_card(&format!(
        r#"
      <h1>Browser client limit reached</h1>
      <p>This server login already has {} measurement clients.
        It cannot approve another client yet.</p>
      <p>Renewing this login revokes its existing client grants and ends
        their active measurements.</p>
      <p><a href="/login">Renew this server's login</a>, then return to
        the requesting Graphite Meter interface and choose Sign in again.</p>
      "#,
        super::grant::MAX_SESSION_GRANTS
    ))
}

fn approval_card(card: &str) -> String {
    page("", "Approve client", "card", &format!("      {card}\n"), true)
}

pub fn done_page(browser: bool) -> String {
    let client = if browser { "Browser client" } else { "Terminal client" };
    page(
        "",
        "Client approved",
        "card completion",
        &format!(
            r#"      <svg class="mark" viewBox="0 0 24 24" aria-hidden="true">{HEX}{CHECK}</svg>
      <p class="eyebrow">{client}</p>
      <h1>Client approved</h1>
      <p>Your client can now receive measurement access. You can close this tab and return to Graphite Meter.</p>
"#
        ),
        false,
    )
}

pub fn continue_page(challenge: &str, opening: bool) -> String {
    let (refresh, link): (String, String) = if challenge.is_empty() {
        ("/".into(), "/".into())
    } else {
        let mut query = String::new();
        for byte in challenge.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                query.push(byte.into());
            } else {
                write!(query, "%{byte:02x}").expect("string writer");
            }
        }
        (
            format!("/auth/cli?challenge={}", escape(challenge)),
            format!("/auth/cli?challenge={query}"),
        )
    };
    let (mark, heading) = if opening {
        (String::new(), "Continue sign-in")
    } else {
        (
            format!(r#"<svg class="mark" viewBox="0 0 24 24" aria-hidden="true">{HEX}{CHECK}</svg>"#),
            "Signed in",
        )
    };
    page(
        &format!("    <meta http-equiv=\"refresh\" content=\"0; url={refresh}\">\n"),
        "Signing in",
        "card completion",
        &format!(
            r#"      {mark}
      <h1>{heading}</h1>
      <p>Taking you to Graphite Meter…</p>
      <p><a href="{link}">Continue</a></p>
"#
        ),
        false,
    )
}

fn page(head: &str, title: &str, class: &str, main: &str, pending: bool) -> String {
    let pending = if pending {
        format!("    <script>{PENDING_SCRIPT}</script>\n")
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width,initial-scale=1">
    <link rel="icon" href="{ICON}">
{head}    <title>{title} · Graphite Meter</title>
    <script>{THEME_SCRIPT}</script>
    <style>{STYLES}</style>
  </head>
  <body>
    <main class="{class}">
{main}    </main>
{pending}  </body>
</html>
"#
    )
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
pub fn security_headers(authorization_origin: Option<&str>) -> Result<HeaderMap, InvalidAuthorizationOrigin> {
    let mut csp = CSP.clone();
    if let Some(origin) = authorization_origin.filter(|origin| !origin.is_empty()) {
        if !super::secure_browser_origin(origin)
            || origin
                .chars()
                .any(|ch| ch.is_whitespace() || matches!(ch, ';' | '\'' | '"' | '*' | '\\' | '<' | '>'))
        {
            return Err(InvalidAuthorizationOrigin);
        }
        csp = csp.replace("form-action 'self'", &format!("form-action 'self' {origin}"));
    }
    let mut headers = HeaderMap::new();
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
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
