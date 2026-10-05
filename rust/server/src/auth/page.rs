//! The sign-in and approval pages, rendered from Go's templates in `go/internal/auth/assets` as its html/template
//! renders them, and the headers of every authentication answer.

use http::{HeaderMap, HeaderValue, header};
use std::{fmt::Write as _, sync::LazyLock};

const STYLES: &str = include_str!("../../../../go/internal/auth/assets/auth.css");
const THEME: &str = include_str!("../../../../go/internal/auth/assets/theme.js");
const PENDING: &str = include_str!("../../../../go/internal/auth/assets/pending.js");
static TEMPLATES: LazyLock<[Template; 4]> = LazyLock::new(|| {
    [
        include_str!("../../../../go/internal/auth/assets/login.tmpl"),
        include_str!("../../../../go/internal/auth/assets/cli.tmpl"),
        include_str!("../../../../go/internal/auth/assets/cli-done.tmpl"),
        include_str!("../../../../go/internal/auth/assets/continue.tmpl"),
    ]
    .map(Template::parse)
});

/// The pages, in `TEMPLATES` order.
#[derive(Clone, Copy)]
pub(super) enum Page {
    Login,
    /// An approval, its refusal or the client limit.
    Cli,
    /// An approval's confirmation.
    CliDone,
    /// The step back from a provider or another site to this one.
    Continue,
}

/// `page` with `fields` set, such as `("CSRF", token)`; a flag is `"true"` or empty.
pub(super) fn render(page: Page, fields: &[(&str, &str)]) -> String {
    TEMPLATES[page as usize].render(fields)
}

/// Lets the page's forms post to the OIDC provider's authorization `origin` as well.
pub(super) fn allow_form_action(headers: &mut HeaderMap, origin: &str) {
    let policy = headers
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|policy| policy.to_str().ok());
    let policy = policy.unwrap_or_default();
    let policy = policy.replace("form-action 'self'", &format!("form-action 'self' {origin}"));
    if let Ok(policy) = HeaderValue::from_str(&policy) {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
}

/// Go's headers of every authentication page and refusal, with HSTS once the request is known to be secure.
pub(super) fn protect(headers: &mut HeaderMap, secure: bool) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    static POLICY: LazyLock<HeaderValue> = LazyLock::new(|| {
        let hash = |asset: &str| STANDARD.encode(Sha256::digest(asset));
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
    crate::app::finalize::harden(headers, secure);
    headers.insert(header::CONTENT_SECURITY_POLICY, POLICY.clone());
}

/// A template's actions, each with the text after it; the first action is empty.
struct Template(Vec<(String, String)>);

impl Template {
    /// Splits `source` at its actions, without the HTML comments html/template drops.
    fn parse(source: &str) -> Self {
        let mut source = source.to_owned();
        while let Some(start) = source.find("<!--") {
            let end = source[start..].find("-->").map_or(source.len(), |end| start + end + 3);
            source.replace_range(start..end, "");
        }
        let mut parts = source.split("{{");
        let first = (String::new(), parts.next().unwrap_or_default().to_owned());
        let actions = parts.map(|part| {
            let (action, text) = part.split_once("}}").expect("templates close their actions");
            (action.to_owned(), text.to_owned())
        });
        Self(std::iter::once(first).chain(actions).collect())
    }

    /// The actions these templates use: fields, escaped for a URL query inside an `href`; the theme and pending
    /// scripts; and `if`, `else if`, `else` and `end` on `not`, `and`, `eq` and `ne`.
    fn render(&self, fields: &[(&str, &str)]) -> String {
        let field = |name: &str| {
            let name = name.trim_start_matches('.');
            fields
                .iter()
                .find(|(key, _)| *key == name)
                .map_or("", |(_, value)| *value)
        };
        let (mut page, mut branches) = (String::new(), Vec::<(bool, bool)>::new());
        let shown = |branches: &[(bool, bool)]| branches.iter().all(|&(_, on)| on);
        for (action, text) in &self.0 {
            if let Some(test) = action.strip_prefix("if ") {
                let on = condition(test, &field);
                branches.push((on, on));
            } else if let Some(rest) = action.strip_prefix("else") {
                let (taken, on) = branches.last_mut().expect("an else follows an if");
                *on = !*taken && rest.strip_prefix(" if ").is_none_or(|test| condition(test, &field));
                *taken |= *on;
            } else if action == "end" {
                branches.pop();
            } else if shown(&branches) {
                match action.as_str() {
                    "" => {}
                    "template \"theme\"" => {
                        page.push_str(concat!(
                            "<meta name=\"theme-color\" media=\"(prefers-color-scheme: dark)\" content=\"#0d1013\">",
                            "<meta name=\"theme-color\" media=\"(prefers-color-scheme: light)\" content=\"#f0f3f6\">"
                        ));
                        page.extend(["<script>", THEME, "</script>"]);
                    }
                    "template \"pending\"" => page.extend(["<script>", PENDING, "</script>"]),
                    ".Styles" => page.push_str(STYLES),
                    name if page.rfind("href=\"").is_some_and(|at| !page[at + 6..].contains('"')) => {
                        for byte in field(name).bytes() {
                            match byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                                true => page.push(byte.into()),
                                false => write!(page, "%{byte:02x}").expect("strings take writes"),
                            }
                        }
                    }
                    name => escape(&mut page, field(name)),
                }
            }
            if shown(&branches) {
                page.push_str(text);
            }
        }
        page
    }
}

/// Whether a condition holds: a field or flag is true when nonempty.
fn condition<'a>(test: &'a str, field: &impl Fn(&'a str) -> &'a str) -> bool {
    fn value<'a>(words: &mut impl Iterator<Item = &'a str>, field: &impl Fn(&'a str) -> &'a str) -> &'a str {
        let flag = |on: bool| if on { "true" } else { "" };
        match words.next().expect("conditions are complete") {
            "not" => flag(value(words, field).is_empty()),
            operator @ ("and" | "eq" | "ne") => {
                let (first, second) = (value(words, field), value(words, field));
                flag(match operator {
                    "and" => !first.is_empty() && !second.is_empty(),
                    "eq" => first == second,
                    _ => first != second,
                })
            }
            quoted if quoted.starts_with('"') => quoted.trim_matches('"'),
            name => field(name),
        }
    }
    let mut words = test.split(['(', ')', ' ']).filter(|word| !word.is_empty());
    !value(&mut words, field).is_empty()
}

/// html/template's escaping of text and quoted attribute values.
fn escape(page: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '\0' => page.push('\u{fffd}'),
            '"' => page.push_str("&#34;"),
            '&' => page.push_str("&amp;"),
            '\'' => page.push_str("&#39;"),
            '+' => page.push_str("&#43;"),
            '<' => page.push_str("&lt;"),
            '>' => page.push_str("&gt;"),
            character => page.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_page_shows_the_notice_and_form_its_fields_choose() {
        let login = |fields: &[(&str, &str)]| render(Page::Login, fields);
        let page = login(&[("CSRF", "a+b\"<"), ("Notice", "password"), ("Password", "true"), ("Provider", "Id")]);
        assert!(page.contains("<input type=\"hidden\" name=\"csrf\" value=\"a&#43;b&#34;&lt;\">"));
        assert!(page.contains("Incorrect password. Check it and try again."));
        assert!(!page.contains("Too many attempts") && !page.contains("Sign-in failed"));
        assert!(page.contains("action=\"/auth/password\"") && !page.contains("/auth/oidc/start"));
        assert!(page.contains("autocomplete=\"current-password\" autofocus required"));
        assert!(!page.contains("<!--") && !page.replace(PENDING, "").contains("{{"));
        assert!(
            page.contains(&format!("<style>{STYLES}</style>")) && page.contains(&format!("<script>{PENDING}</script>"))
        );
        let page = login(&[("Notice", "other"), ("OIDC", "true"), ("Provider", "Id"), ("Password", "true")]);
        assert!(page.contains("Sign-in failed. Try again."));
        assert!(page.contains("<button type=\"submit\" disabled>Continue with Id</button>"));
        assert!(page.contains("<p class=\"notice\">Id is unavailable right now.</p>"));
        assert!(page.contains("<div class=\"separator\">or</div>"));
        let page = login(&[("Notice", "provider"), ("OIDC", "true"), ("OIDCReady", "true"), ("Provider", "Id")]);
        assert!(page.contains("Id is unavailable right now.</p>") && !page.contains("operator password"));
        assert!(page.contains("<button type=\"submit\" >Continue with Id</button>"));
    }
}
