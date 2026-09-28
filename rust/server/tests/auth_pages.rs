use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_server::auth::pages::{
    LoginPage, PENDING_SCRIPT, STYLES, THEME_SCRIPT, approval_page, capacity_page, continue_page, done_page,
    security_headers,
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

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Case {
    page: String,
    csrf: String,
    provider: String,
    challenge: String,
    notice: String,
    status: String,
    code: String,
    origin: String,
    password: bool,
    oidc: bool,
    oidc_ready: bool,
    browser: bool,
    capacity: bool,
    opening: bool,
}

fn render(case: &Case) -> String {
    match case.page.as_str() {
        "login" => LoginPage {
            csrf: &case.csrf,
            provider: &case.provider,
            challenge: &case.challenge,
            password: case.password,
            oidc: case.oidc,
            oidc_ready: case.oidc_ready,
            notice: &case.notice,
            status: &case.status,
        }
        .render(),
        "cli" if case.capacity => capacity_page(),
        "cli" => approval_page(&case.code, &case.csrf, &case.challenge, &case.origin),
        "cli-done" => done_page(case.browser),
        "continue" => continue_page(&case.challenge, case.opening),
        page => panic!("unknown page {page}"),
    }
}

#[test]
fn pages_render_as_go_renders_the_shared_goldens() {
    let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/../../go/internal/auth/testdata/pages");
    let mut pages = 0;
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let golden = std::fs::read_to_string(&path).unwrap();
        let (header, expected) = golden.split_once('\n').unwrap();
        let case: Case = serde_json::from_str(header).unwrap();
        let html = render(&case)
            .replace(STYLES, "/* auth.css */")
            .replace(THEME_SCRIPT, "/* theme.js */")
            .replace(PENDING_SCRIPT, "/* pending.js */");
        assert_eq!(html, expected, "{}", path.display());
        pages += 1;
    }
    assert!(pages >= 15, "{pages} golden pages");
}

#[test]
fn every_inline_asset_matches_csp_hash_of_actual_rendered_bytes() {
    let login = LoginPage {
        csrf: "csrf-token",
        provider: "Authelia",
        challenge: "challenge",
        password: true,
        oidc: true,
        oidc_ready: true,
        notice: "",
        status: "",
    };
    let pages = [
        (login.render(), true),
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
        assert_eq!(
            scripts,
            if pending {
                vec![THEME_SCRIPT, PENDING_SCRIPT]
            } else {
                vec![THEME_SCRIPT]
            }
        );
        for asset in styles.into_iter().chain(scripts) {
            let expected = format!("'sha256-{}'", STANDARD.encode(Sha256::digest(asset.as_bytes())));
            assert!(csp.contains(&expected));
        }
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

#[tokio::test]
async fn application_response_restricts_resources_and_hashes_embedded_inline_assets() {
    use graphite_meter_server::config::{Config, NativeKind};
    use graphite_meter_server::http_server::HttpServer;
    use std::{sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    };

    tokio::time::timeout(Duration::from_secs(5), async {
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve(NativeKind::H1, listener, None, async {
            let _ = stopped.await;
        }));
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket
            .write_all(format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).await.unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
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
            assert!(
                policy.split("; ").any(|actual| actual == directive),
                "missing {directive}: {policy}"
            );
        }
        for tag in ["script", "style"] {
            let mut expected = format!("{tag}-src 'self'");
            for inline in inline_blocks(body, tag) {
                expected.push_str(&format!(
                    " 'sha256-{}'",
                    STANDARD.encode(Sha256::digest(inline.as_bytes()))
                ));
            }
            assert!(policy.split("; ").any(|actual| actual == expected), "{policy}");
        }
        assert!(!policy.contains("unsafe-inline"));
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
