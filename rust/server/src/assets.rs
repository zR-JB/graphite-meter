//! The embedded browser app: exact paths, the index with the server's meta tags, the browser's notice from the
//! shared report, and the page's content security policy.

#[cfg(test)]
mod scan;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use graphite_meter_legal::{BROWSER_NOTICE, Notices};
use sha2::{Digest, Sha256};

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
    pub content_type: &'static str,
    pub bytes: Bytes,
    pub cache: Option<&'static str>,
}

/// The browser app as one configuration serves it.
pub struct Assets {
    files: &'static [Asset],
    notices: &'static Notices,
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
        Self { files, notices, index, inline }
    }

    /// The file at `path`, a request path without its query: `/` is the index, and only exact names match.
    pub fn get(&self, path: &str) -> Option<Served> {
        if path == "/" {
            let bytes = self.index.clone()?;
            return Some(Served {
                content_type: "text/html; charset=utf-8",
                bytes,
                cache: Some("no-store"),
            });
        }
        let name = path.strip_prefix('/').filter(|name| *name != "index.html")?;
        if name == BROWSER_NOTICE {
            let bytes = Bytes::from_static(self.notices.browser()?);
            return Some(Served {
                content_type: "text/plain; charset=utf-8",
                bytes,
                cache: None,
            });
        }
        let file = self.files.iter().find(|file| file.path == name)?;
        // The bundler names every file under assets/ by its content hash.
        let cache = name
            .starts_with("assets/")
            .then_some("public, max-age=31536000, immutable");
        Some(Served {
            content_type: file.content_type,
            bytes: Bytes::from_static(file.bytes),
            cache,
        })
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
