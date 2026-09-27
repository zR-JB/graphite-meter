//! Exact-path browser assets embedded at build time; no runtime filesystem access.
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

struct EmbeddedAsset {
    path: &'static str,
    content_type: &'static str,
    bytes: &'static [u8],
}
include!(concat!(env!("OUT_DIR"), "/browser_assets.rs"));
include!(concat!(env!("OUT_DIR"), "/legal.rs"));

/// The reviewed legal report, inflated on first use; the browser's notice is its suffix.
pub fn legal_report() -> Option<&'static [u8]> {
    static REPORT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    REPORT
        .get_or_init(|| {
            LEGAL.map(|(compressed, length)| {
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, length)
                    .ok()
                    .filter(|report| report.len() == length)
                    .expect("the build embeds a complete legal report")
            })
        })
        .as_deref()
}

pub struct Assets {
    entries: &'static [EmbeddedAsset],
    index: Option<Bytes>,
    inline_script_hash: Option<String>,
    inline_style_hash: Option<String>,
}

impl Assets {
    /// Precompute the configured shell once at server startup. Builds without
    /// GM_RUST_ASSET_DIR contain no browser assets and report unavailable.
    pub fn new(auth_enabled: bool, history_default: bool) -> Self {
        Self::from_entries(EMBEDDED, auth_enabled, history_default)
    }

    fn from_entries(entries: &'static [EmbeddedAsset], auth_enabled: bool, history_default: bool) -> Self {
        let index = entries.iter().find(|entry| entry.path == "index.html");
        let inline_script_hash = index.and_then(|entry| inline_hash(entry.bytes, "script"));
        let inline_style_hash = index.and_then(|entry| inline_hash(entry.bytes, "style"));
        let index = index.map(|entry| {
            let mut marker = String::new();
            if auth_enabled {
                marker.push_str("<meta name=\"graphite-meter-auth\" content=\"enabled\">");
            }
            let history_marker =
                format!("<meta name=\"graphite-meter-result-history-default\" content=\"{history_default}\">");
            marker.push_str(&history_marker);
            let html = std::str::from_utf8(entry.bytes).expect("build validated UTF-8 index");
            Bytes::from(html.replacen("</head>", &format!("{marker}</head>"), 1))
        });
        Self {
            entries,
            index,
            inline_script_hash,
            inline_style_hash,
        }
    }

    pub fn available(&self) -> bool {
        self.index.is_some()
    }

    /// Base64 SHA-256 digest, without CSP quotes or the sha256- prefix.
    pub fn inline_script_hash(&self) -> Option<&str> {
        self.inline_script_hash.as_deref()
    }

    pub fn inline_style_hash(&self) -> Option<&str> {
        self.inline_style_hash.as_deref()
    }

    /// `path` is the request URI path, excluding its query. No decoding,
    /// normalization, directory listing or arbitrary SPA fallback is performed.
    pub fn serve(&self, method: &Method, path: &str, headers: &HeaderMap) -> Response<Bytes> {
        if method != Method::GET && method != Method::HEAD {
            let mut response = text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            return response;
        }
        let found = if path == "/" {
            self.index
                .clone()
                .map(|index| ("text/html; charset=utf-8", Some("no-store"), index))
        } else {
            path.strip_prefix('/')
                .filter(|name| *name != "index.html")
                .and_then(|name| {
                    let (content_type, bytes) = self.asset(name)?;
                    // The bundler names every file under assets/ by its content hash.
                    let cache = name
                        .starts_with("assets/")
                        .then_some("public, max-age=31536000, immutable");
                    Some((content_type, cache, Bytes::from_static(bytes)))
                })
        };
        let mut response = match found {
            Some((content_type, cache, body)) => {
                let mut response = content(headers, content_type, body);
                if let Some(cache) = cache.filter(|_| response.status() != StatusCode::RANGE_NOT_SATISFIABLE) {
                    response
                        .headers_mut()
                        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
                }
                response
            }
            None => text(StatusCode::NOT_FOUND, "404 page not found\n"),
        };
        if method == Method::HEAD {
            *response.body_mut() = Bytes::new();
        }
        response
    }

    fn asset(&self, name: &str) -> Option<(&'static str, &'static [u8])> {
        if name == "legal/THIRD_PARTY_NOTICES.txt" {
            return Some(("text/plain; charset=utf-8", &legal_report()?[LEGAL_NOTICES?..]));
        }
        let entry = self.entries.iter().find(|entry| entry.path == name)?;
        Some((entry.content_type, entry.bytes))
    }
}

fn text(status: StatusCode, body: &'static str) -> Response<Bytes> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_LENGTH, body.len())
        .body(Bytes::from_static(body.as_bytes()))
        .expect("static response headers are valid")
}

/// Go's `http.ServeContent` without ETag or modification time, so `If-Range` never matches.
fn content(headers: &HeaderMap, content_type: &str, body: Bytes) -> Response<Bytes> {
    let any_tag = |name| {
        let tags = headers
            .get(name)?
            .to_str()
            .ok()
            .filter(|tags| !tags.trim().is_empty())?;
        Some(tags.split(',').any(|tag| tag.trim() == "*"))
    };
    let status = match (any_tag(header::IF_MATCH), any_tag(header::IF_NONE_MATCH)) {
        (Some(false), _) => Some(StatusCode::PRECONDITION_FAILED),
        (_, Some(true)) => Some(StatusCode::NOT_MODIFIED),
        _ => None,
    };
    if let Some(status) = status {
        let mut response = Response::new(Bytes::new());
        *response.status_mut() = status;
        return response;
    }
    let size = body.len() as u64;
    let range = headers
        .get(header::RANGE)
        .filter(|_| !headers.contains_key(header::IF_RANGE))
        .map_or(Some(""), |range| range.to_str().ok());
    let ranges = match range.map(|range| ranges(range, size)) {
        Some(Ok(ranges)) if ranges.iter().map(|(_, length)| length).sum::<u64>() <= size => ranges,
        // Go ignores ranges that add up past the content, and any range of empty content.
        Some(Ok(_)) => Vec::new(),
        Some(Err(true)) if size == 0 => Vec::new(),
        Some(Err(true)) => {
            let mut response = text(StatusCode::RANGE_NOT_SATISFIABLE, "invalid range: failed to overlap\n");
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}")).expect("numeric range"),
            );
            return response;
        }
        Some(Err(false)) | None => return text(StatusCode::RANGE_NOT_SATISFIABLE, "invalid range\n"),
    };
    let content_range =
        |start: u64, length: u64| format!("bytes {start}-{}/{size}", start as i128 + length as i128 - 1);
    let mut response = Response::new(body.clone());
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("known type"),
    );
    match ranges[..] {
        [] => {}
        [(start, length)] => {
            *response.body_mut() = body.slice(start as usize..(start + length) as usize);
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&content_range(start, length)).expect("numeric range"),
            );
        }
        _ => {
            let mut random = [0_u8; 30];
            let _ = getrandom::fill(&mut random);
            let boundary: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let mut parts = Vec::new();
            for (index, &(start, length)) in ranges.iter().enumerate() {
                let separator = if index == 0 { "" } else { "\r\n" };
                parts.extend_from_slice(
                    format!(
                        "{separator}--{boundary}\r\nContent-Range: {}\r\nContent-Type: {content_type}\r\n\r\n",
                        content_range(start, length)
                    )
                    .as_bytes(),
                );
                parts.extend_from_slice(&body[start as usize..(start + length) as usize]);
            }
            parts.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
            *response.body_mut() = parts.into();
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}")).expect("hex boundary"),
            );
        }
    }
    let length = response.body().len();
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// Go's `parseRange`: `(start, length)` pairs, or whether the failure is only a lack of overlap.
fn ranges(range: &str, size: u64) -> Result<Vec<(u64, u64)>, bool> {
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let specs = range.strip_prefix("bytes=").ok_or(false)?;
    let (mut ranges, mut no_overlap) = (Vec::new(), false);
    for spec in specs.split(',').map(str::trim).filter(|spec| !spec.is_empty()) {
        let (start, end) = spec.split_once('-').ok_or(false)?;
        let (start, end) = (start.trim(), end.trim());
        let number = |text: &str| text.parse::<i64>().ok().and_then(|value| u64::try_from(value).ok());
        if start.is_empty() {
            let suffix = number(end).filter(|_| !end.starts_with('-')).ok_or(false)?;
            ranges.push((size - suffix.min(size), suffix.min(size)));
            continue;
        }
        let start = number(start).ok_or(false)?;
        if start >= size {
            no_overlap = true;
            continue;
        }
        let end = match end {
            "" => size - 1,
            end => number(end).filter(|&end| end >= start).ok_or(false)?.min(size - 1),
        };
        ranges.push((start, end - start + 1));
    }
    if no_overlap && ranges.is_empty() {
        return Err(true);
    }
    Ok(ranges)
}

fn inline_hash(html: &[u8], tag: &str) -> Option<String> {
    let html = std::str::from_utf8(html).ok()?;
    let (_, content) = html.split_once(&format!("<{tag}>"))?;
    let (content, _) = content.split_once(&format!("</{tag}>"))?;
    Some(STANDARD.encode(Sha256::digest(content.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    static FILES: &[EmbeddedAsset] = &[
        EmbeddedAsset {
            path: "index.html",
            content_type: "text/html; charset=utf-8",
            bytes:
                b"<html><head><style>html { background: #131518; }\n</style><script>const theme = 'dark';\n</script></head><body></body></html>",
        },
        EmbeddedAsset {
            path: "assets/app.js",
            content_type: "text/javascript; charset=utf-8",
            bytes: b"export {};",
        },
        EmbeddedAsset {
            path: "favicon.svg",
            content_type: "image/svg+xml",
            bytes: b"<svg/>",
        },
    ];

    #[test]
    fn shell_metadata_and_head_length_are_precomputed() {
        let assets = Assets::from_entries(FILES, true, true);
        assert!(assets.available());
        let get = assets.serve(&Method::GET, "/", &HeaderMap::new());
        let body = std::str::from_utf8(get.body()).unwrap();
        assert!(body.contains("name=\"graphite-meter-auth\" content=\"enabled\""));
        assert!(body.contains("name=\"graphite-meter-result-history-default\" content=\"true\""));
        assert_eq!(get.headers()["cache-control"], "no-store");
        assert_eq!(get.headers()["x-content-type-options"], "nosniff");
        let head = assets.serve(&Method::HEAD, "/", &HeaderMap::new());
        assert!(head.body().is_empty());
        assert_eq!(head.headers()["content-length"], get.body().len().to_string());
        let security = crate::app_security::AppSecurity::new(
            std::sync::Arc::new(crate::config::Config::default()),
            assets.inline_script_hash(),
            assets.inline_style_hash(),
        )
        .unwrap();
        let headers = security.headers("localhost").unwrap();
        let policy = headers["content-security-policy"].to_str().unwrap();
        for tag in ["script", "style"] {
            let content = body
                .split_once(&format!("<{tag}>"))
                .unwrap()
                .1
                .split_once(&format!("</{tag}>"))
                .unwrap()
                .0;
            let hash = STANDARD.encode(Sha256::digest(content.as_bytes()));
            assert!(
                policy
                    .split("; ")
                    .any(|directive| directive == format!("{tag}-src 'self' 'sha256-{hash}'"))
            );
        }
        assert!(!policy.contains("unsafe-inline"));
        let public = Assets::from_entries(FILES, false, false).serve(&Method::GET, "/", &HeaderMap::new());
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
                assets.serve(&Method::GET, path, &HeaderMap::new()).status(),
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        let js = assets.serve(&Method::GET, "/assets/app.js", &HeaderMap::new());
        assert_eq!(js.status(), StatusCode::OK);
        assert_eq!(js.headers()["content-type"], "text/javascript; charset=utf-8");
        assert_eq!(js.body(), "export {};");
        let head = assets.serve(&Method::HEAD, "/assets/app.js", &HeaderMap::new());
        assert!(head.body().is_empty());
        assert_eq!(head.headers(), js.headers());
        let post = assets.serve(&Method::POST, "/assets/app.js", &HeaderMap::new());
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(post.headers()["allow"], "GET, HEAD");
    }

    #[test]
    fn hashed_assets_are_immutable_and_ranges_follow_go() {
        let assets = Assets::from_entries(FILES, false, false);
        let get = |path, headers: &[(header::HeaderName, &'static str)]| {
            let headers = headers
                .iter()
                .map(|(name, value)| (name.clone(), HeaderValue::from_static(value)))
                .collect();
            assets.serve(&Method::GET, path, &headers)
        };
        let js = get("/assets/app.js", &[]);
        assert_eq!(js.headers()["cache-control"], "public, max-age=31536000, immutable");
        assert_eq!(js.headers()["accept-ranges"], "bytes");
        assert!(!get("/favicon.svg", &[]).headers().contains_key("cache-control"));
        let part = get("/assets/app.js", &[(header::RANGE, "bytes=0-3")]);
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.headers()["content-range"], "bytes 0-3/10");
        assert_eq!(part.body(), "expo");
        let parts = get("/assets/app.js", &[(header::RANGE, "bytes=0-1, -2")]);
        let body = std::str::from_utf8(parts.body()).unwrap();
        assert!(
            parts.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("multipart/byteranges; boundary=")
        );
        assert!(body.contains("Content-Range: bytes 0-1/10\r\nContent-Type: text/javascript; charset=utf-8\r\n\r\nex"));
        assert!(body.contains("Content-Range: bytes 8-9/10\r\n") && body.ends_with("--\r\n"));
        let past = get("/assets/app.js", &[(header::RANGE, "bytes=20-")]);
        assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(past.headers()["content-range"], "bytes */10");
        assert_eq!(past.body(), "invalid range: failed to overlap\n");
        assert!(!past.headers().contains_key("cache-control"));
        assert_eq!(
            get("/assets/app.js", &[(header::RANGE, "items=0-1")]).body(),
            "invalid range\n"
        );
        let stale = get(
            "/assets/app.js",
            &[(header::RANGE, "bytes=0-3"), (header::IF_RANGE, "\"v1\"")],
        );
        assert_eq!((stale.status(), stale.body().len()), (StatusCode::OK, 10));
        assert_eq!(
            get("/assets/app.js", &[(header::IF_NONE_MATCH, "*")]).status(),
            StatusCode::NOT_MODIFIED
        );
        assert_eq!(
            get("/assets/app.js", &[(header::IF_MATCH, "\"v1\"")]).status(),
            StatusCode::PRECONDITION_FAILED
        );
    }
}
