//! The browser app on UI listeners: embedded files, index meta tags, the browser's notice and the page policy.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use graphite_meter_legal::Notices;
use graphite_meter_server::assets::{Asset, Assets};
use http::{HeaderMap, StatusCode};
use sha2::{Digest, Sha256};

const INDEX: &str = "<html><head><style>html{background:#131518}</style><script>let theme='dark';</script></head>\
                     <body></body></html>";

const fn asset(path: &'static str, content_type: &'static str, bytes: &'static [u8]) -> Asset {
    Asset { path, content_type, bytes }
}

const SCRIPT: &str = "text/javascript; charset=utf-8";

static FILES: &[Asset] = &[
    asset("index.html", "text/html; charset=utf-8", INDEX.as_bytes()),
    asset("assets/app-1a2b.js", SCRIPT, b"export {};"),
    asset("assets/app-1a2b.js.br", SCRIPT, b"brotli"),
    asset("assets/app-1a2b.js.gz", SCRIPT, b"gzip"),
    asset("fonts/face.woff2", "font/woff2", b"wOF2"),
    asset("favicon.svg", "image/svg+xml", b"<svg/>"),
];

const REPORT: &str = "project\nrust crates\nbrowser notice\n";

/// The report with the browser's notice as its suffix.
fn notices() -> &'static Notices {
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(REPORT.as_bytes(), 9).leak();
    let browser = REPORT.len() - "browser notice\n".len();
    Box::leak(Box::new(Notices::new(Some((compressed, REPORT.len())), Some(browser), "")))
}

fn served(env: &[(&str, &str)]) -> App {
    app(env).with_assets(FILES, notices())
}

async fn get(app: &App, method: &str, path: &str, headers: &[(&str, &str)]) -> Response<Body> {
    let request = headers
        .iter()
        .fold(request(method, path), |request, (name, value)| request.header(*name, *value));
    send(app, Endpoint::H1, empty(request)).await
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
    let auth = Assets::new(FILES, notices(), true, false)
        .get("/", &HeaderMap::new())
        .unwrap()
        .bytes;
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
        (Some("image/svg+xml"), Some("no-cache"))
    );
    for path in [
        "/index.html",
        "/assets",
        "/assets/",
        "/%69ndex.html",
        "//favicon.svg",
        "/assets/../index.html",
        "/assets/app-1a2b.js.br",
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

/// A client gets the build's brotli or gzip copy it accepts, tagged per encoding, and revalidates by tag.
#[tokio::test]
async fn a_client_gets_the_copy_it_accepts_and_revalidates_by_tag() {
    let app = served(&[]);
    let mut tags = Vec::new();
    for (accept, body, encoding) in [
        ("", "export {};", None),
        ("gzip, deflate", "gzip", Some("gzip")),
        ("gzip, deflate, br, zstd", "brotli", Some("br")),
        ("br;q=0, gzip;q=0.5", "gzip", Some("gzip")),
        ("identity", "export {};", None),
    ] {
        let response = get(&app, "GET", "/assets/app-1a2b.js", &[("accept-encoding", accept)]).await;
        assert_eq!(header(&response, "content-encoding"), encoding, "{accept}");
        assert_eq!(header(&response, "vary"), Some("Accept-Encoding"));
        assert_eq!(header(&response, "content-type"), Some("text/javascript; charset=utf-8"));
        assert_eq!(header(&response, "content-length"), Some(body.len().to_string().as_str()));
        tags.push((encoding, header(&response, "etag").unwrap().to_owned()));
        assert_eq!(text(response).await, body);
    }
    tags.sort();
    tags.dedup();
    assert_eq!(tags.len(), 3);
    let head = get(&app, "HEAD", "/assets/app-1a2b.js", &[("accept-encoding", "br")]).await;
    assert_eq!(header(&head, "content-length"), Some("6"));
    let held = format!("\"other\", W/{}", tags[2].1);
    let accept = ("accept-encoding", "gzip");
    let unchanged = get(&app, "GET", "/assets/app-1a2b.js", &[accept, ("if-none-match", &held)]).await;
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(header(&unchanged, "vary"), Some("Accept-Encoding"));
    for name in ["content-encoding", "content-type", "content-length"] {
        assert_eq!(header(&unchanged, name), None, "{name}");
    }
    assert_eq!(header(&unchanged, "etag"), Some(tags[2].1.as_str()));
    assert_eq!(header(&unchanged, "cache-control"), Some("public, max-age=31536000, immutable"));
    assert_eq!(text(unchanged).await, "");
    let changed = get(&app, "GET", "/favicon.svg", &[("if-none-match", "\"other\"")]).await;
    assert_eq!(text(changed).await, "<svg/>");
    let font = get(&app, "GET", "/fonts/face.woff2", &[]).await;
    assert_eq!(header(&font, "cache-control"), Some("public, max-age=604800"));
    let index = get(&app, "GET", "/", &[("accept-encoding", "br, gzip")]).await;
    for name in ["content-encoding", "vary", "etag"] {
        assert_eq!(header(&index, name), None, "{name}");
    }
    assert!(text(index).await.contains("content=\"false\"></head>"));
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
