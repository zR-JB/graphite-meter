//! The browser app on UI listeners: embedded files, index meta tags, the browser's notice and the page policy.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use graphite_meter_legal::Notices;
use graphite_meter_server::assets::{Asset, Assets};
use http::StatusCode;
use sha2::{Digest, Sha256};

const INDEX: &str = "<html><head><style>html{background:#131518}</style><script>let theme='dark';</script></head>\
                     <body></body></html>";

static FILES: &[Asset] = &[
    Asset {
        path: "index.html",
        content_type: "text/html; charset=utf-8",
        bytes: INDEX.as_bytes(),
    },
    Asset {
        path: "assets/app-1a2b.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: b"export {};",
    },
    Asset {
        path: "favicon.svg",
        content_type: "image/svg+xml",
        bytes: b"<svg/>",
    },
];

const REPORT: &str = "project\nrust crates\nbrowser notice\n";

/// The report with the browser's notice as its suffix.
fn notices() -> &'static Notices {
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(REPORT.as_bytes(), 9).leak();
    let browser = REPORT.len() - "browser notice\n".len();
    Box::leak(Box::new(Notices::new(Some((compressed, REPORT.len())), Some(browser), false)))
}

fn served(env: &[(&str, &str)]) -> App {
    app(env).with_assets(FILES, notices())
}

fn sha256(text: &str) -> String {
    STANDARD.encode(Sha256::digest(text.as_bytes()))
}

#[tokio::test]
async fn the_index_carries_the_server_meta_tags_under_the_page_policy() {
    let app = served(&[("GM_RESULT_HISTORY_DEFAULT", "true")]);
    let response = send(&app, Endpoint::H1, empty(request("GET", "/"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    for (name, value) in [
        ("content-type", "text/html; charset=utf-8"),
        ("cache-control", "no-store"),
        ("x-frame-options", "DENY"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "same-origin"),
    ] {
        assert_eq!(header(&response, name), Some(value), "{name}");
    }
    let policy = header(&response, "content-security-policy").unwrap().to_owned();
    let (script, style) = (sha256("let theme='dark';"), sha256("html{background:#131518}"));
    let expected = format!(
        "default-src 'self'; script-src 'self' 'sha256-{script}'; style-src 'self' 'sha256-{style}'; \
         img-src 'self' data:; font-src 'self'; worker-src 'self'; object-src 'none'; base-uri 'none'; \
         form-action 'self'; frame-ancestors 'none'; connect-src 'self' http://speed.example:7246 \
         ws://speed.example:7246"
    );
    assert_eq!(policy, expected);
    let meta = "<meta name=\"graphite-meter-result-history-default\" content=\"true\"></head>";
    assert_eq!(text(response).await, INDEX.replace("</head>", meta));
    let off = served(&[]);
    let index = text(send(&off, Endpoint::H1, empty(request("GET", "/"))).await).await;
    assert!(index.contains("<meta name=\"graphite-meter-result-history-default\" content=\"false\"></head>"));
    assert!(!index.contains("graphite-meter-auth"));
    let auth = Assets::new(FILES, notices(), true, false).get("/").unwrap().bytes;
    let tags = "<meta name=\"graphite-meter-auth\" content=\"enabled\">\
                <meta name=\"graphite-meter-result-history-default\" content=\"false\"></head>";
    assert!(std::str::from_utf8(&auth).unwrap().contains(tags));
}

#[tokio::test]
async fn only_exact_embedded_names_are_served_on_ui_listeners() {
    let app = served(&ALL_LISTENERS);
    let get = |endpoint, path| send(&app, endpoint, empty(request("GET", path)));
    let script = get(Endpoint::H1Tls, "/assets/app-1a2b.js").await;
    assert_eq!(header(&script, "content-type"), Some("text/javascript; charset=utf-8"));
    assert_eq!(header(&script, "cache-control"), Some("public, max-age=31536000, immutable"));
    assert_eq!(text(script).await, "export {};");
    let icon = get(Endpoint::H1Tls, "/favicon.svg").await;
    assert_eq!(
        (header(&icon, "content-type"), header(&icon, "cache-control")),
        (Some("image/svg+xml"), None)
    );
    for path in [
        "/index.html",
        "/assets",
        "/assets/",
        "/%69ndex.html",
        "//favicon.svg",
        "/assets/../index.html",
    ] {
        let response = get(Endpoint::H1Tls, path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(header(&response, "content-security-policy").is_some());
    }
    for endpoint in [Endpoint::H2, Endpoint::H3Companion, Endpoint::Quic] {
        let response = get(endpoint, "/").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{endpoint:?}");
        assert_eq!(header(&response, "content-security-policy"), None);
    }
}

#[tokio::test]
async fn head_keeps_the_length_and_other_methods_are_not_allowed() {
    let app = served(&[]);
    let head = send(&app, Endpoint::H1, empty(request("HEAD", "/favicon.svg"))).await;
    assert_eq!(header(&head, "content-length"), Some("6"));
    assert_eq!(text(head).await, "");
    for (method, path) in [("POST", "/favicon.svg"), ("PUT", "/"), ("POST", "/preflight"), ("OPTIONS", "/ws/ping")] {
        let response = send(&app, Endpoint::H1, empty(request(method, path))).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        assert_eq!(header(&response, "allow"), Some("GET, HEAD"));
        assert_eq!(text(response).await, "method not allowed\n");
    }
}

#[tokio::test]
async fn the_browser_notice_is_the_shared_report_suffix() {
    let app = served(&[]);
    let response = send(&app, Endpoint::H1, empty(request("GET", "/legal/THIRD_PARTY_NOTICES.txt"))).await;
    assert_eq!(header(&response, "content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(text(response).await, "browser notice\n");
    let unshared = Box::leak(Box::new(Notices::new(None, None, false)));
    let unshared = super::app(&[]).with_assets(FILES, unshared);
    let response = send(&unshared, Endpoint::H1, empty(request("GET", "/legal/THIRD_PARTY_NOTICES.txt"))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_page_connects_to_catalogue_servers_and_offered_targets() {
    let catalog = r#"{"servers": [
        {"id": "other", "url": "https://other.example", "name": "Other",
         "additionalOrigins": ["https://cdn.other.example:8443"]},
        {"id": "v6", "url": "https://[2001:db8::1]", "name": "V6", "additionalOrigins": ["http://[2001:db8::2]"]}
    ]}"#;
    let env = [
        ("GM_TLS_CERT", "/cert.pem"),
        ("GM_TLS_KEY", "/key.pem"),
        ("GM_H3_ADDR", ":7249"),
        ("GM_PUBLIC_ORIGINS", "self"),
        ("GM_SERVER_CATALOG", catalog),
    ];
    let app = served(&env);
    let response = send(&app, Endpoint::H1, empty(request("GET", "/"))).await;
    let policy = header(&response, "content-security-policy").unwrap();
    let connect = policy.rsplit("; ").next().unwrap();
    let expected = "connect-src 'self' http://other.example:* https://other.example:* ws://other.example:* \
                    wss://other.example:* https://cdn.other.example:8443 wss://cdn.other.example:8443 \
                    http://speed.example:7246 https://speed.example:7249 ws://speed.example:7246 \
                    wss://speed.example:7249";
    assert_eq!(connect, expected);
}
