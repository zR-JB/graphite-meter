//! The embedded browser app: tagged paths, precompressed copies, the index's meta tags, the browser notice, the CSP.

#[cfg(test)]
mod scan;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use bytes::Bytes;
use graphite_meter_legal::{BROWSER_NOTICE, Notices};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::OnceLock};

/// One embedded file.
pub struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));
include!(concat!(env!("OUT_DIR"), "/legal.rs"));

/// A file as served.
pub struct Served {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: Bytes,
}

/// An embedded file with its content tag.
struct Tagged {
    asset: &'static Asset,
    tag: HeaderValue,
}

/// A served name's file and the build's brotli and gzip copies of it, in that preference.
struct File {
    plain: Tagged,
    encoded: Vec<(&'static str, Tagged)>,
}

/// The browser app as one configuration serves it.
pub struct Assets {
    files: HashMap<&'static str, File>,
    notices: &'static Notices,
    notice_tag: OnceLock<HeaderValue>,
    /// The index with the server's meta tags.
    index: Option<Bytes>,
    /// The policy's script and style sources, which admit the index's inline elements.
    inline: [String; 2],
}

impl Assets {
    /// The app and notices this build embeds; a build without `GM_RUST_ASSET_DIR` has no app.
    pub fn embedded(auth: bool, history_default: bool) -> Self {
        Self::new(EMBEDDED, &NOTICES, auth, history_default)
    }

    pub fn new(files: &'static [Asset], notices: &'static Notices, auth: bool, history_default: bool) -> Self {
        let index = files
            .iter()
            .find(|file| file.path == "index.html")
            .map(|file| file.bytes);
        let inline = [("script", "script-src"), ("style", "style-src")].map(|(tag, directive)| {
            match index.and_then(|html| inline_hash(html, tag)) {
                Some(hash) => format!("{directive} 'self' 'sha256-{hash}'"),
                None => format!("{directive} 'self'"),
            }
        });
        let auth = if auth {
            "<meta name=\"graphite-meter-auth\" content=\"enabled\">"
        } else {
            ""
        };
        let meta = format!("{auth}<meta name=\"graphite-meter-result-history-default\" content=\"{history_default}\">");
        let index = index.map(|html| {
            let html = std::str::from_utf8(html).expect("the build checked the index");
            Bytes::from(html.replacen("</head>", &format!("{meta}</head>"), 1))
        });
        let tagged = |asset: &'static Asset| Tagged { asset, tag: tag(asset.bytes) };
        let copy = |path: String| files.iter().find(|file| file.path == path).map(tagged);
        let files = files
            .iter()
            .filter(|file| !matches!(file.path.rsplit_once('.'), Some((_, "br" | "gz"))) && file.path != "index.html")
            .map(|file| {
                let encoded = [("br", ".br"), ("gzip", ".gz")]
                    .into_iter()
                    .filter_map(|(coding, suffix)| Some((coding, copy(format!("{}{suffix}", file.path))?)))
                    .collect();
                (file.path, File { plain: tagged(file), encoded })
            })
            .collect();
        Self { files, notices, notice_tag: OnceLock::new(), index, inline }
    }

    /// The file at `path`, a request path without its query, as `request` asks for it: `/` is the index, only exact
    /// names match, and a client gets the precompressed copy it accepts or a 304 for the tag it holds.
    pub fn get(&self, path: &str, request: &HeaderMap) -> Option<Served> {
        let text = |name: header::HeaderName| {
            request
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
        };
        let mut headers = HeaderMap::new();
        let (content_type, cache, bytes, tag) = if path == "/" {
            ("text/html; charset=utf-8", "no-store", self.index.clone()?, None)
        } else if path.strip_prefix('/') == Some(BROWSER_NOTICE) {
            let bytes = self.notices.browser()?;
            let tag = self.notice_tag.get_or_init(|| tag(bytes));
            ("text/plain; charset=utf-8", "no-cache", Bytes::from_static(bytes), Some(tag))
        } else {
            let name = path.strip_prefix('/')?;
            let file = self.files.get(name)?;
            // The bundler names every file under assets/ by its content hash; the fonts are unmodified upstream
            // faces, reused for a week before a reload checks their tag.
            let cache = match name {
                _ if name.starts_with("assets/") => "public, max-age=31536000, immutable",
                _ if name.starts_with("fonts/") => "public, max-age=604800",
                _ => "no-cache",
            };
            if !file.encoded.is_empty() {
                headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
            }
            let accept = text(header::ACCEPT_ENCODING);
            let served = match file.encoded.iter().find(|(coding, _)| accepts(accept, coding)) {
                Some((coding, copy)) => {
                    headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static(coding));
                    copy
                }
                None => &file.plain,
            };
            let Tagged { asset, tag } = served;
            (asset.content_type, cache, Bytes::from_static(asset.bytes), Some(tag))
        };
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        if let Some(tag) = tag {
            headers.insert(header::ETAG, tag.clone());
        }
        if none_match(text(header::IF_NONE_MATCH), tag) {
            headers.remove(header::CONTENT_ENCODING);
            return Some(Served {
                status: StatusCode::NOT_MODIFIED,
                headers,
                bytes: Bytes::new(),
            });
        }
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        Some(Served { status: StatusCode::OK, headers, bytes })
    }

    /// The page's content security policy, connecting to `connect` beyond 'self'.
    pub fn policy(&self, connect: &[String]) -> String {
        let [script, style] = &self.inline;
        let connect = connect
            .iter()
            .fold("connect-src 'self'".to_owned(), |all, source| all + " " + source);
        [
            "default-src 'self'",
            script,
            style,
            "img-src 'self' data:",
            "font-src 'self'",
            "worker-src 'self'",
            "object-src 'none'",
            "base-uri 'none'",
            "form-action 'self'",
            "frame-ancestors 'none'",
            &connect,
        ]
        .join("; ")
    }
}

/// The base64 SHA-256 of the first attribute-less inline `tag`'s text.
fn inline_hash(html: &[u8], tag: &str) -> Option<String> {
    let html = std::str::from_utf8(html).ok()?;
    let (_, content) = html.split_once(&format!("<{tag}>"))?;
    let (content, _) = content.split_once(&format!("</{tag}>"))?;
    Some(STANDARD.encode(Sha256::digest(content.as_bytes())))
}

/// The quoted base64 of the first 12 bytes of the SHA-256 of `bytes`.
fn tag(bytes: &[u8]) -> HeaderValue {
    let tag = format!("\"{}\"", URL_SAFE_NO_PAD.encode(&Sha256::digest(bytes)[..12]));
    HeaderValue::try_from(tag).expect("base64 is a valid header value")
}

/// Whether an Accept-Encoding header admits `coding` with a nonzero weight, read as Go's static handler reads it.
fn accepts(header: &str, coding: &str) -> bool {
    header
        .split(',')
        .find_map(|part| {
            let (token, params) = part.split_once(';').unwrap_or((part, ""));
            let params = params.trim();
            let weight = params.strip_prefix("q=").unwrap_or(params).parse::<f64>();
            token
                .trim()
                .eq_ignore_ascii_case(coding)
                .then(|| weight.ok().is_none_or(|q| q > 0.0))
        })
        .unwrap_or(false)
}

/// Whether an If-None-Match header names `tag` or any representation, scanned as Go's `net/http` scans it.
fn none_match(mut header: &str, tag: Option<&HeaderValue>) -> bool {
    loop {
        header = header.trim_start_matches([' ', '\t', '\r', '\n', ',']);
        if header.starts_with('*') {
            return true;
        }
        let Some(quoted) = header.strip_prefix("W/").unwrap_or(header).strip_prefix('"') else {
            return false;
        };
        let Some(end) = quoted.find(|c: char| c.is_ascii() && c != '!' && !('#'..='~').contains(&c)) else {
            return false;
        };
        if !quoted[end..].starts_with('"') {
            return false;
        }
        if tag.is_some_and(|tag| tag.as_bytes()[1..] == quoted.as_bytes()[..=end]) {
            return true;
        }
        header = &quoted[end + 1..];
    }
}
