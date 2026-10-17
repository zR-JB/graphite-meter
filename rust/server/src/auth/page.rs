//! The sign-in and approval pages around the assets `client/auth/build.ts` writes, and authentication answer headers.

use super::approval::{Challenge, Refused};
use graphite_meter_proto::route::AuthRoute;
use http::{HeaderMap, HeaderValue, header};
use std::sync::LazyLock;

const STYLES: &str = include_str!("assets/auth.css");
const THEME: &str = include_str!("assets/theme.js");
const PENDING: &str = include_str!("assets/pending.js");
const ICON: &str = include_str!("assets/icon.html");
const BRAND: &str = include_str!("assets/brand.html");

/// The sign-in page.
pub(super) struct Login<'a> {
    pub csrf: &'a str,
    pub provider: &'a str,
    pub challenge: Option<&'a str>,
    /// The `error` query: a `Reason::notice`, anything else unnamed.
    pub error: &'a str,
    /// The `reason` query.
    pub reason: &'a str,
    pub password: bool,
    pub oidc: bool,
    pub oidc_ready: bool,
}

impl Login<'_> {
    pub(super) fn render(&self) -> String {
        let provider = escape(self.provider);
        let unavailable = format!("{provider} is unavailable right now.");
        let mut main = String::from(FACE);
        let status = match self.reason {
            "signed_out" => "You are signed out.",
            "expired" => "Your session ended. Sign in again.",
            "renew" => "Sign in again before starting this long test.",
            _ => "",
        };
        if !status.is_empty() {
            main += &format!("<p class=\"notice\" data-tone=\"neutral\" role=\"status\">{status}</p>\n");
        }
        let (tone, alert) = match self.error {
            "" => ("", String::new()),
            "password" => ("err", "Incorrect password. Check it and try again.".into()),
            "throttled" => ("warn", "Too many attempts. Wait a minute and try again.".into()),
            "proxy" => (
                "err",
                "This server cannot tell your address from its reverse proxy. Ask its operator to check the proxy's \
                 X-Real-IP."
                    .into(),
            ),
            "provider" if self.password => ("warn", format!("{unavailable} Sign in with the operator password.")),
            "provider" => ("warn", unavailable.clone()),
            "busy" => ("warn", "The server is busy. Try again in a moment.".into()),
            "stale" => ("warn", "This sign-in form expired. Try again.".into()),
            _ => ("err", "Sign-in failed. Try again.".into()),
        };
        let mistyped = self.error == "password";
        if !alert.is_empty() {
            let id = if mistyped { " id=\"password-error\"" } else { "" };
            main += &format!("<p class=\"notice\"{id} data-tone=\"{tone}\" role=\"alert\">{alert}</p>\n");
        }
        let fields = format!(
            "<input type=\"hidden\" name=\"csrf\" value=\"{}\">\n<input type=\"hidden\" name=\"challenge\" value=\"{}\">",
            escape(self.csrf),
            escape(self.challenge.unwrap_or_default()),
        );
        if self.oidc {
            let disabled = if self.oidc_ready { "" } else { " disabled" };
            main += &format!(
                "<form method=\"post\" action=\"{}\">\n{fields}\n<button type=\"submit\"{disabled}>Continue with \
                 {provider}</button>\n</form>\n",
                AuthRoute::OidcStart.path()
            );
            if !self.oidc_ready && self.error != "provider" {
                main += &format!("<p class=\"notice\" data-tone=\"neutral\">{unavailable}</p>\n");
            }
        }
        if self.oidc && self.password {
            main += "<div class=\"separator\">or</div>\n";
        }
        if self.password {
            let invalid = if mistyped {
                " aria-invalid=\"true\" aria-describedby=\"password-error\""
            } else {
                ""
            };
            // Beside a working provider the caret stays out of the operator's field.
            let autofocus = if self.oidc && self.oidc_ready { "" } else { " autofocus" };
            // The username field names the one account for password managers; the server ignores it.
            main += &format!(
                r#"<form method="post" action="{action}">
{fields}
<input class="sr-only" name="username" type="text" value="operator" autocomplete="username" readonly tabindex="-1" aria-hidden="true">
<div class="field">
<label for="password">Operator password</label>
<div class="entry">
<input id="password" name="password" type="password"{invalid} maxlength="1024" autocomplete="current-password" autocapitalize="none" autocorrect="off" spellcheck="false"{autofocus} required>
<button class="reveal" type="button" data-reveal="password" aria-label="Show password" aria-pressed="false" hidden><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z"/><circle cx="12" cy="12" r="3"/><path class="strike" d="M4 4l16 16"/></svg></button>
</div>
</div>
<button type="submit">Sign in</button>
</form>
"#,
                action = AuthRoute::Password.path()
            );
        }
        page("Sign in", "", "card", &main, true)
    }
}

/// The locked meter above the sign-in form.
const FACE: &str = r#"<div class="face" aria-hidden="true"><svg viewBox="0 0 120 104"><path class="ticks" d="M23.94 96.06L21.11 98.89M9.98 69.95L6.06 70.73M12.88 40.48L9.19 38.95M31.67 17.60L29.44 14.27M60 9L60 5M88.33 17.60L90.56 14.27M107.12 40.48L110.81 38.95M110.02 69.95L113.94 70.73M96.06 96.06L98.89 98.89"/><path class="track" d="M28.89 91.11A44 44 0 1 1 91.11 91.11"/><path class="sweep" d="M28.89 91.11A44 44 0 1 1 91.11 91.11" pathLength="1"/><g class="needle"><circle cx="28.89" cy="91.11" r="5"/></g><path class="lock" d="M54 55v-5a6 6 0 0 1 12 0v5M51.5 55h17v12h-17z"/></svg></div>
<h1>Sign in</h1>
<p>This measurement server is private.</p>
<p class="host" data-host></p>
"#;

/// The approval page: an approval to confirm, a refusal, or a login at its client limit.
pub(super) enum Cli<'a> {
    Approval {
        code: &'a str,
        challenge: &'a str,
        csrf: &'a str,
        /// The browser origin asking, or none for a terminal.
        browser_origin: Option<&'a str>,
    },
    Refused(Refused),
    Capacity {
        limit: usize,
    },
}

impl Cli<'_> {
    pub(super) fn render(&self) -> String {
        let (title, main) = match self {
            Self::Refused(refused) => {
                // One fixed sentence per refusal kind: nothing from the request is echoed.
                let why = match refused {
                    Refused::Busy => "Too many approvals are open. Try again from the client in a few minutes.",
                    Refused::Link => "This approval link is not valid. Start sign-in again from the client.",
                };
                ("Approval unavailable", format!("<h1>Approval unavailable</h1>\n<p>{why}</p>\n"))
            }
            Self::Capacity { limit } => (
                "Client limit reached",
                format!(
                    "<h1>Client limit reached</h1>\n<p>This sign-in already has {limit} approved clients, the most it \
                     can have.</p>\n<p>Renewing the sign-in ends their access and any tests they are running.</p>\n\
                     <p><a href=\"{}\">Renew this sign-in</a>, then choose Sign in again on the site that asked.</p>\n",
                    AuthRoute::Login.path()
                ),
            ),
            Self::Approval { code, challenge, csrf, browser_origin } => {
                let (title, intro, action) = match browser_origin {
                    Some(origin) => (
                        "Approve browser client",
                        format!(
                            "<p>Approve only a site you opened yourself. The site below can then run tests as you \
                             until you sign out or this sign-in expires.</p>\n<p class=\"origin\"><strong>{}</strong>\
                             </p>\n<p>Confirm that this code matches the code on that site.</p>",
                            escape(origin)
                        ),
                        AuthRoute::BrowserApprove,
                    ),
                    None => (
                        "Approve terminal client",
                        "<p>Approve only a terminal you started yourself. It can then run tests as you until you sign \
                         out or this sign-in expires.</p>\n<p>Confirm that this code matches the code in your \
                         terminal.</p>"
                            .into(),
                        AuthRoute::CliApprove,
                    ),
                };
                let main = format!(
                    "<h1>{title}</h1>\n{intro}\n<code class=\"code\">{}</code>\n<form method=\"post\" action=\"{}\">\n\
                     <input type=\"hidden\" name=\"csrf\" value=\"{}\">\n<input type=\"hidden\" name=\"challenge\" \
                     value=\"{}\">\n<button type=\"submit\">Approve this client</button>\n</form>\n",
                    escape(code),
                    action.path(),
                    escape(csrf),
                    escape(challenge),
                );
                (title, main)
            }
        };
        page(title, "", "card", &main, true)
    }
}

/// An approval's confirmation, for a browser or a terminal.
pub(super) fn cli_done(browser: bool) -> String {
    let (title, text) = match browser {
        true => ("Browser client approved", "The site you approved can now run tests as you."),
        false => ("Terminal client approved", "Your terminal can now run tests as you."),
    };
    let main = format!("<h1>{title}</h1>\n<p>{text} You can close this tab and return to it.</p>\n");
    page(title, "", "card completion", &main, false)
}

/// The step back from a provider or another site to this one, opening the approval when `opening`.
pub(super) fn continuing(challenge: Option<&Challenge>, opening: bool) -> String {
    let title = if opening { "Continue sign-in" } else { "Signed in" };
    let (next, place) = match challenge {
        Some(challenge) => (escape(&challenge.page()), "the approval page"),
        None => ("/".into(), "Graphite&nbsp;Meter"),
    };
    let head = format!("<meta http-equiv=\"refresh\" content=\"0; url={next}\">\n");
    let main = format!("<h1>{title}</h1>\n<p>Taking you to {place}…</p>\n<p><a href=\"{next}\">Continue</a></p>\n");
    page(title, &head, "card completion", &main, false)
}

/// A whole page: `title` before the product name, `head` and `main` markup, and the pending script beside forms.
fn page(title: &str, head: &str, class: &str, main: &str, forms: bool) -> String {
    let pending = if forms { format!("<script>{PENDING}</script>\n") } else { String::new() };
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
{ICON}
{head}<title>{title} · Graphite Meter</title>
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="#0d1013">
<meta name="theme-color" media="(prefers-color-scheme: light)" content="#f1f2f5">
<script>{THEME}</script>
<style>{STYLES}</style>
</head>
<body>
<main class="{class}">
{BRAND}
{main}</main>
{pending}</body>
</html>
"##
    )
}

/// Lets the page's forms post to the OIDC provider's authorization `origin` as well.
pub(super) fn allow_form_action(headers: &mut HeaderMap, origin: &str) {
    let declared = headers.get(header::CONTENT_SECURITY_POLICY);
    let policy = declared.and_then(|policy| policy.to_str().ok());
    let policy = policy.unwrap_or_default();
    let policy = policy.replace("form-action 'self'", &format!("form-action 'self' {origin}"));
    if let Ok(policy) = HeaderValue::from_str(&policy) {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
}

/// The headers of every authentication page and refusal, with HSTS once the request is known to be secure.
pub(super) fn protect(headers: &mut HeaderMap, secure: bool) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ring::digest::{SHA256, digest};
    static POLICY: LazyLock<HeaderValue> = LazyLock::new(|| {
        let hash = |asset: &str| STANDARD.encode(digest(&SHA256, asset.as_bytes()));
        let (styles, theme, pending) = (hash(STYLES), hash(THEME), hash(PENDING));
        let policy = format!(
            "default-src 'none'; style-src 'sha256-{styles}'; font-src 'self'; script-src 'sha256-{theme}' \
             'sha256-{pending}'; connect-src 'self'; img-src data:; form-action 'self'; frame-ancestors 'none'; \
             base-uri 'none'"
        );
        HeaderValue::from_str(&policy).expect("hashes are base64")
    });
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // A private server's sign-in and approval pages stay out of search indexes.
    headers.insert(crate::headers::X_ROBOTS_TAG, HeaderValue::from_static("noindex, nofollow"));
    crate::app::finalize::harden(headers, secure);
    headers.insert(header::CONTENT_SECURITY_POLICY, POLICY.clone());
}

/// `text` safe in HTML text and quoted attribute values.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("&#34;"),
            '&' => escaped.push_str("&amp;"),
            '\'' => escaped.push_str("&#39;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_page_shows_the_notice_and_form_its_fields_choose() {
        let page = Login {
            csrf: "a+b\"<",
            provider: "Id",
            challenge: None,
            error: "password",
            reason: "",
            password: true,
            oidc: false,
            oidc_ready: false,
        }
        .render();
        assert!(page.contains("<input type=\"hidden\" name=\"csrf\" value=\"a+b&#34;&lt;\">"));
        assert!(page.contains("Incorrect password. Check it and try again."));
        assert!(!page.contains("Too many attempts") && !page.contains("Sign-in failed"));
        assert!(page.contains("action=\"/auth/password\"") && !page.contains("/auth/oidc/start"));
        assert!(page.contains("spellcheck=\"false\" autofocus required"));
        assert!(
            page.contains(&format!("<style>{STYLES}</style>")) && page.contains(&format!("<script>{PENDING}</script>"))
        );
    }
}
