use askama::Template;
use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_server::auth::pages::{
    ApprovalPage, ContinuePage, DonePage, LoginPage, PENDING_SCRIPT, STYLES, THEME_SCRIPT,
    security_headers,
};
use sha2::{Digest, Sha256};

fn login() -> LoginPage<'static> {
    LoginPage {
        csrf: "csrf-token",
        provider: "Authelia",
        challenge: "challenge",
        password: true,
        oidc: true,
        oidc_ready: true,
        notice: "",
        status: "",
    }
}

fn inline_blocks<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    html.split(&open)
        .skip(1)
        .map(|part| part.split_once(&close).unwrap().0)
        .collect()
}

#[test]
fn every_inline_asset_matches_csp_hash_of_actual_rendered_bytes() {
    let pages = [
        (login().render().unwrap(), true),
        (
            ApprovalPage {
                browser_capacity: false,
                client_limit: 8,
                browser_origin: "",
                code: "1234",
                csrf: "csrf",
                challenge: "challenge",
            }
            .render()
            .unwrap(),
            true,
        ),
        (DonePage { browser: true }.render().unwrap(), false),
        (
            ContinuePage {
                challenge: "",
                opening: false,
            }
            .render()
            .unwrap(),
            false,
        ),
    ];
    let headers = security_headers(None).unwrap();
    let csp = headers["content-security-policy"].to_str().unwrap();
    for (html, pending) in pages {
        let styles = inline_blocks(&html, "style");
        assert_eq!(styles, [STYLES]);
        let scripts = inline_blocks(&html, "script");
        assert_eq!(
            scripts,
            if pending {
                vec![THEME_SCRIPT, PENDING_SCRIPT]
            } else {
                vec![THEME_SCRIPT]
            }
        );
        for asset in styles.into_iter().chain(scripts) {
            let expected = format!(
                "'sha256-{}'",
                STANDARD.encode(Sha256::digest(asset.as_bytes()))
            );
            assert!(csp.contains(&expected));
        }
        assert_eq!(html.matches("rel=\"icon\"").count(), 1);
    }
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "same-origin");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["permissions-policy"],
        "camera=(), microphone=(), geolocation=()"
    );
    assert!(!csp.contains("unsafe-inline"));
}

#[test]
fn dynamic_fields_are_html_escaped_and_never_become_scripts() {
    let attack = "\"><script>alert('x')</script>&";
    let mut page = login();
    page.csrf = attack;
    page.challenge = attack;
    page.provider = attack;
    page.notice = "provider";
    let html = page.render().unwrap();
    assert!(!html.contains(attack));
    assert!(!html.contains("<script>alert"));
    assert_eq!(
        inline_blocks(&html, "script"),
        [THEME_SCRIPT, PENDING_SCRIPT]
    );
    let approval = ApprovalPage {
        browser_capacity: false,
        client_limit: 8,
        browser_origin: attack,
        code: attack,
        csrf: attack,
        challenge: attack,
    }
    .render()
    .unwrap();
    assert!(!approval.contains(attack));
    assert_eq!(
        inline_blocks(&approval, "script"),
        [THEME_SCRIPT, PENDING_SCRIPT]
    );
    assert!(approval.contains("action=\"/auth/browser/approve\""));
}

#[test]
fn login_notices_statuses_and_auth_methods_preserve_branches() {
    for (notice, expected) in [
        ("password", "Incorrect password."),
        ("throttled", "Too many attempts."),
        ("provider", "Authelia is unreachable"),
        ("busy", "The server is busy."),
        ("stale", "This sign-in form expired."),
        ("unexpected", "Sign-in failed."),
    ] {
        let mut page = login();
        page.notice = notice;
        assert!(page.render().unwrap().contains(expected));
    }
    for (status, expected) in [
        ("signed_out", "You're signed out."),
        ("expired", "Your session ended."),
        ("renew", "Sign in again before starting this long test."),
    ] {
        let mut page = login();
        page.status = status;
        assert!(page.render().unwrap().contains(expected));
    }
    let mut page = login();
    page.password = false;
    page.oidc_ready = false;
    let html = page.render().unwrap();
    assert!(!html.contains("action=\"/auth/password\""));
    assert!(html.contains("type=\"submit\" disabled"));
    assert!(html.contains("Authelia is temporarily unavailable."));
    assert!(!html.contains("class=\"separator\""));
    page.password = true;
    page.oidc = false;
    let html = page.render().unwrap();
    assert!(!html.contains("action=\"/auth/oidc/start\""));
    assert!(html.contains("autocomplete=\"current-password\""));
}

#[test]
fn approval_capacity_and_completion_keep_expected_forms_and_scripts() {
    let mut page = ApprovalPage {
        browser_capacity: false,
        client_limit: 8,
        browser_origin: "",
        code: "1234",
        csrf: "csrf",
        challenge: "challenge",
    };
    let terminal = page.render().unwrap();
    assert!(terminal.contains("Approve terminal client"));
    assert!(terminal.contains("action=\"/auth/cli/approve\""));
    page.browser_origin = "https://meter.example";
    assert!(page.render().unwrap().contains("Approve browser client"));
    page.browser_capacity = true;
    let capacity = page.render().unwrap();
    assert!(capacity.contains("already has 8 measurement clients"));
    assert!(!capacity.contains("<form"));
    for browser in [false, true] {
        let html = DonePage { browser }.render().unwrap();
        assert!(html.contains(if browser {
            "Browser client"
        } else {
            "Terminal client"
        }));
        assert!(!html.contains("<form"));
        assert_eq!(inline_blocks(&html, "script"), [THEME_SCRIPT]);
    }
}

#[test]
fn continue_encodes_challenge_as_one_query_value() {
    let page = ContinuePage {
        challenge: "a&next=https://evil.example/\"<script>;#",
        opening: true,
    };
    let destination = page.destination();
    let parsed = url::Url::parse(&format!("https://meter.example{destination}")).unwrap();
    assert_eq!(parsed.path(), "/auth/cli");
    assert_eq!(
        parsed.query_pairs().collect::<Vec<_>>(),
        [("challenge".into(), page.challenge.into())]
    );
    let html = page.render().unwrap();
    assert!(html.contains("<h1>Continue sign-in</h1>"));
    assert!(!html.contains("<svg class=\"mark\""));
    assert!(!html.contains(page.challenge));
    let page = ContinuePage {
        challenge: "",
        opening: false,
    };
    assert_eq!(page.destination(), "/");
    assert!(page.render().unwrap().contains("<h1>Signed in</h1>"));
}

#[test]
fn oidc_csp_widens_only_form_action_to_validated_origin() {
    let headers = security_headers(Some("https://identity.example:8443")).unwrap();
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("form-action 'self' https://identity.example:8443;"));
    assert_eq!(csp.matches("https://identity.example:8443").count(), 1);
    for origin in [
        "http://identity.example",
        "https://identity.example/path",
        "https://identity.example;script-src *",
        "https://*.example",
        "https://identity.example\n",
    ] {
        assert!(security_headers(Some(origin)).is_err(), "{origin:?}");
    }
}
