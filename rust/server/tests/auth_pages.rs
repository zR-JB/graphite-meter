#[path = "support/http1.rs"]
mod http1;

#[path = "support/native.rs"]
mod native;

use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_server::auth::pages::{
    LoginPage, PENDING_SCRIPT, STYLES, THEME_SCRIPT, approval_page, continue_page, done_page, security_headers,
};
use sha2::{Digest, Sha256};

fn inline_blocks<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    html.split(&open)
        .skip(1)
        .map(|part| part.split_once(&close).unwrap().0)
        .collect()
}

fn hash(asset: &str) -> String {
    format!("'sha256-{}'", STANDARD.encode(Sha256::digest(asset.as_bytes())))
}

/// A login page offering both methods, with every field the caller's.
fn login<'a>(text: &'a str, notice: &'a str, status: &'a str) -> LoginPage<'a> {
    LoginPage {
        csrf: text,
        provider: text,
        challenge: text,
        password: true,
        oidc: true,
        oidc_ready: true,
        notice,
        status,
    }
}

#[test]
fn fields_are_escaped_for_html_attributes_and_urls() {
    let hostile = "A&B <i>\"x\"</i> 'y'+z\0";
    let escaped = "A&amp;B &lt;i&gt;&#34;x&#34;&lt;/i&gt; &#39;y&#39;&#43;z\u{fffd}";
    let approval = approval_page(hostile, hostile, hostile, hostile);
    assert!(approval.contains(escaped));
    assert!(!approval.contains(hostile));
    let login = login(hostile, "password", "signed_out").render();
    assert!(login.contains(escaped) && !login.contains(hostile));
    let continued = continue_page("a b&c+d/é\"<>'\n", true);
    assert!(continued.contains("a%20b%26c%2bd%2f%c3%a9%22%3c%3e%27%0a"));
}

#[test]
fn every_inline_asset_matches_csp_hash_of_actual_rendered_bytes() {
    let pages = [
        (login("csrf-token", "", "").render(), true),
        (approval_page("1234", "csrf", "challenge", ""), true),
        (done_page(true), false),
        (continue_page("", false), false),
    ];
    let headers = security_headers(None).unwrap();
    let csp = headers["content-security-policy"].to_str().unwrap();
    for (html, pending) in pages {
        let styles = inline_blocks(&html, "style");
        assert_eq!(styles, [STYLES]);
        let scripts = inline_blocks(&html, "script");
        let expected: &[&str] = if pending {
            &[THEME_SCRIPT, PENDING_SCRIPT]
        } else {
            &[THEME_SCRIPT]
        };
        assert_eq!(scripts, expected);
        for asset in styles.into_iter().chain(scripts) {
            assert!(csp.contains(&hash(asset)));
        }
    }
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "same-origin");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let permissions = &headers["permissions-policy"];
    assert_eq!(permissions, "camera=(), microphone=(), geolocation=()");
    assert!(!csp.contains("unsafe-inline"));
    assert_eq!(csp.matches("font-src 'self'").count(), 1);
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
        assert!(security_headers(Some(origin)).is_none(), "{origin:?}");
    }
}

#[tokio::test]
async fn application_response_restricts_resources_and_hashes_embedded_inline_assets() {
    use graphite_meter_server::config::{Config, NativeKind};
    use graphite_meter_server::http::HttpServer;
    use std::{sync::Arc, time::Duration};

    tokio::time::timeout(Duration::from_secs(5), async {
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let listener = native::serve(server, NativeKind::H1, None).await;
        let address = listener.address;
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (headers, body) = http1::exchange(socket, "GET", "/", &address.to_string(), "", b"").await;
        let body = String::from_utf8(body).unwrap();
        let policy = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-security-policy: "))
            .unwrap();
        for directive in [
            "default-src 'self'",
            "img-src 'self' data:",
            "font-src 'self'",
            "worker-src 'self'",
            "object-src 'none'",
            "base-uri 'none'",
            "form-action 'self'",
            "frame-ancestors 'none'",
        ] {
            let found = policy.split("; ").any(|actual| actual == directive);
            assert!(found, "missing {directive}: {policy}");
        }
        for tag in ["script", "style"] {
            let mut expected = format!("{tag}-src 'self'");
            for inline in inline_blocks(&body, tag) {
                expected = expected + " " + &hash(inline);
            }
            assert!(policy.split("; ").any(|actual| actual == expected), "{policy}");
        }
        assert!(!policy.contains("unsafe-inline"));
        listener.shutdown().await;
    })
    .await
    .unwrap();
}
