//! Exact-path browser assets embedded at build time; no runtime filesystem access.
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{Method, Response, StatusCode};
use sha2::{Digest, Sha256};

struct EmbeddedAsset {
    path: &'static str,
    content_type: &'static str,
    bytes: &'static [u8],
}
include!(concat!(env!("OUT_DIR"), "/browser_assets.rs"));

/// The reviewed release notice is also part of the browser's About assets.
/// The CLI reuses these bytes so the executable contains only one copy.
pub fn legal_notices() -> Option<&'static [u8]> {
    EMBEDDED
        .iter()
        .find(|entry| entry.path == "legal/THIRD_PARTY_NOTICES.txt")
        .map(|entry| entry.bytes)
}

pub struct Assets {
    entries: &'static [EmbeddedAsset],
    index: Option<Bytes>,
    inline_script_hash: Option<String>,
}

impl Assets {
    /// Precompute the configured shell once at server startup. Builds without
    /// GM_RUST_ASSET_DIR contain no browser assets and report unavailable.
    pub fn new(auth_enabled: bool, history_default: bool) -> Self {
        Self::from_entries(EMBEDDED, auth_enabled, history_default)
    }

    fn from_entries(
        entries: &'static [EmbeddedAsset],
        auth_enabled: bool,
        history_default: bool,
    ) -> Self {
        let index = entries.iter().find(|entry| entry.path == "index.html");
        let inline_script_hash = index.and_then(|entry| script_hash(entry.bytes));
        let index = index.map(|entry| {
            let mut marker = String::new();
            if auth_enabled {
                marker.push_str("<meta name=\"graphite-meter-auth\" content=\"enabled\">");
            }
            let history_marker = format!(
                "<meta name=\"graphite-meter-result-history-default\" content=\"{history_default}\">"
            );
            marker.push_str(&history_marker);
            let html = std::str::from_utf8(entry.bytes).expect("build validated UTF-8 index");
            Bytes::from(html.replacen("</head>", &format!("{marker}</head>"), 1))
        });
        Self {
            entries,
            index,
            inline_script_hash,
        }
    }

    pub fn available(&self) -> bool {
        self.index.is_some()
    }

    /// Base64 SHA-256 digest, without CSP quotes or the sha256- prefix.
    pub fn inline_script_hash(&self) -> Option<&str> {
        self.inline_script_hash.as_deref()
    }

    /// `path` is the request URI path, excluding its query. No decoding,
    /// normalization, directory listing or arbitrary SPA fallback is performed.
    pub fn serve(&self, method: &Method, path: &str) -> Response<Bytes> {
        if method != Method::GET && method != Method::HEAD {
            return response(
                StatusCode::METHOD_NOT_ALLOWED,
                "text/plain; charset=utf-8",
                "no-store",
                Bytes::from_static(b"method not allowed\n"),
                false,
                true,
            );
        }
        let head = method == Method::HEAD;
        if path == "/" {
            if let Some(index) = &self.index {
                return response(
                    StatusCode::OK,
                    "text/html; charset=utf-8",
                    "no-store",
                    index.clone(),
                    head,
                    false,
                );
            }
        } else if let Some(name) = path.strip_prefix('/')
            && name != "index.html"
            && let Some(entry) = self.entries.iter().find(|entry| entry.path == name)
        {
            return response(
                StatusCode::OK,
                entry.content_type,
                "public, max-age=0, must-revalidate",
                Bytes::from_static(entry.bytes),
                head,
                false,
            );
        }
        response(
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            "no-store",
            Bytes::from_static(b"not found\n"),
            head,
            false,
        )
    }
}

fn response(
    status: StatusCode,
    content_type: &str,
    cache: &str,
    body: Bytes,
    head: bool,
    allow: bool,
) -> Response<Bytes> {
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("cache-control", cache)
        .header("x-content-type-options", "nosniff")
        .header("content-length", body.len());
    if allow {
        builder = builder.header("allow", "GET, HEAD");
    }
    builder
        .body(if head { Bytes::new() } else { body })
        .expect("static response headers are valid")
}

fn script_hash(html: &[u8]) -> Option<String> {
    let html = std::str::from_utf8(html).ok()?;
    let (_, script) = html.split_once("<script>")?;
    let (script, _) = script.split_once("</script>")?;
    Some(STANDARD.encode(Sha256::digest(script.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    static FILES: &[EmbeddedAsset] = &[
        EmbeddedAsset {
            path: "index.html",
            content_type: "text/html; charset=utf-8",
            bytes:
                b"<html><head><script>const theme = 'dark';\n</script></head><body></body></html>",
        },
        EmbeddedAsset {
            path: "assets/app.js",
            content_type: "text/javascript; charset=utf-8",
            bytes: b"export {};",
        },
    ];

    #[test]
    fn shell_metadata_and_head_length_are_precomputed() {
        let assets = Assets::from_entries(FILES, true, true);
        assert!(assets.available());
        let get = assets.serve(&Method::GET, "/");
        let body = std::str::from_utf8(get.body()).unwrap();
        assert!(body.contains("name=\"graphite-meter-auth\" content=\"enabled\""));
        assert!(body.contains("name=\"graphite-meter-result-history-default\" content=\"true\""));
        assert_eq!(get.headers()["cache-control"], "no-store");
        assert_eq!(get.headers()["x-content-type-options"], "nosniff");
        let head = assets.serve(&Method::HEAD, "/");
        assert!(head.body().is_empty());
        assert_eq!(
            head.headers()["content-length"],
            get.body().len().to_string()
        );
        assert_eq!(
            assets.inline_script_hash(),
            Some(
                STANDARD
                    .encode(Sha256::digest(b"const theme = 'dark';\n"))
                    .as_str()
            )
        );
        let public = Assets::from_entries(FILES, false, false).serve(&Method::GET, "/");
        assert!(
            !std::str::from_utf8(public.body())
                .unwrap()
                .contains("graphite-meter-auth")
        );
    }

    #[test]
    fn exact_asset_paths_and_methods_have_no_filesystem_fallback() {
        let assets = Assets::from_entries(FILES, false, false);
        for path in [
            "/index.html",
            "/assets",
            "/assets/",
            "/unknown",
            "/../index.html",
            "/assets/../index.html",
            "/%69ndex.html",
            "//assets/app.js",
            "/assets\\app.js",
            "assets/app.js",
        ] {
            assert_eq!(
                assets.serve(&Method::GET, path).status(),
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        let js = assets.serve(&Method::GET, "/assets/app.js");
        assert_eq!(js.status(), StatusCode::OK);
        assert_eq!(
            js.headers()["content-type"],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(js.body(), "export {};");
        let head = assets.serve(&Method::HEAD, "/assets/app.js");
        assert!(head.body().is_empty());
        assert_eq!(head.headers(), js.headers());
        let post = assets.serve(&Method::POST, "/assets/app.js");
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(post.headers()["allow"], "GET, HEAD");
    }

    #[test]
    fn absent_build_assets_report_unavailable() {
        let assets = Assets::from_entries(&[], false, false);
        assert!(!assets.available());
        assert_eq!(assets.inline_script_hash(), None);
        assert_eq!(
            assets.serve(&Method::GET, "/").status(),
            StatusCode::NOT_FOUND
        );
    }
}
