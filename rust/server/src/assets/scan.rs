//! The browser build's files as the server embeds them: safe names, regular files, known types and a checked index.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// A file to embed.
pub struct File {
    pub name: String,
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// The files under `root` by name; unreviewed builds omit Go's browser notices, reviewed ones serve the report's.
pub fn scan(root: &Path, reviewed: bool) -> Result<Vec<File>, String> {
    let mut paths = Vec::new();
    collect(root, "", &mut paths)?;
    paths.sort();
    let mut files = Vec::new();
    for (name, path) in paths {
        if name == graphite_meter_legal::BROWSER_NOTICE || (name.starts_with("legal/") && !reviewed) {
            continue;
        }
        // The build's brotli and gzip copies take their original's type.
        let original = name
            .strip_suffix(".br")
            .or_else(|| name.strip_suffix(".gz"))
            .unwrap_or(&name);
        let content_type = content_type(original).ok_or_else(|| format!("unsupported browser asset type: {name}"))?;
        let bytes = fs::read(&path).map_err(failed(&path))?;
        if name == "index.html" {
            check_index(&bytes)?;
        }
        files.push(File { name, content_type, bytes });
    }
    if !files.iter().any(|file| file.name == "index.html") {
        return Err("the browser asset directory has no index.html".into());
    }
    Ok(files)
}

/// The regular files below `directory` as names relative to the root; a symlink or special file is refused.
fn collect(directory: &Path, prefix: &str, paths: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(failed(directory))? {
        let entry = entry.map_err(failed(directory))?;
        let name = entry.file_name();
        let safe_byte = |byte: &u8| byte.is_ascii_alphanumeric() || b"-_.".contains(byte);
        let name = match name.to_str() {
            Some(name) if !name.starts_with('.') && name.as_bytes().iter().all(safe_byte) => name,
            _ => return Err(format!("unsafe browser asset name: {prefix}{name:?}")),
        };
        let relative = format!("{prefix}{name}");
        let kind = entry.file_type().map_err(failed(&entry.path()))?;
        if kind.is_dir() {
            collect(&entry.path(), &format!("{relative}/"), paths)?;
        } else if kind.is_file() {
            paths.push((relative, entry.path()));
        } else {
            return Err(format!("browser asset is a symlink or special file: {relative}"));
        }
    }
    Ok(())
}

/// One `</head>`, at most one terminated inline script and style for the policy hashes, and no server-owned meta tags.
fn check_index(bytes: &[u8]) -> Result<(), String> {
    let html = std::str::from_utf8(bytes).map_err(|_| "index.html is not UTF-8")?;
    if html.matches("</head>").count() != 1 {
        return Err("index.html must contain exactly one </head>".into());
    }
    for tag in ["script", "style"] {
        let open = format!("<{tag}>");
        if html.matches(&open).count() > 1 {
            return Err(format!("index.html has more than one inline {tag}"));
        }
        if html
            .split_once(&open)
            .is_some_and(|(_, rest)| !rest.contains(&format!("</{tag}>")))
        {
            return Err(format!("index.html has an unterminated inline {tag}"));
        }
    }
    let owned = ["graphite-meter-auth", "graphite-meter-result-history-default"];
    if owned.iter().any(|name| html.contains(&format!("name=\"{name}\""))) {
        return Err("index.html must not carry the server's auth or result-history meta tags".into());
    }
    Ok(())
}

fn content_type(name: &str) -> Option<&'static str> {
    Some(match name.rsplit_once('.')?.1 {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

fn failed(path: &Path) -> impl Fn(io::Error) -> String {
    move |error| format!("{}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_testkit::Scratch;

    const INDEX: &str = "<html><head><style>a{}</style><script>let a;</script></head><body></body></html>";

    fn fixture() -> Scratch {
        let scratch = Scratch::new().unwrap();
        for (name, contents) in [
            ("index.html", INDEX),
            ("favicon.svg", "<svg/>"),
            ("assets/app-1a2b.js", "export {};"),
            ("assets/app-1a2b.js.br", "brotli"),
            ("assets/app-1a2b.js.gz", "gzip"),
            ("fonts/plex.woff2", "font"),
            ("legal/about.json", "{}"),
            ("legal/THIRD_PARTY_NOTICES.txt", "Go notices"),
        ] {
            scratch.file(name, contents).unwrap();
        }
        scratch
    }

    fn listed(files: &[File]) -> Vec<String> {
        files
            .iter()
            .map(|file| format!("{} {}", file.name, file.content_type))
            .collect()
    }

    #[test]
    fn a_build_embeds_known_files_and_only_reviewed_browser_notices() {
        let scratch = fixture();
        let unreviewed = scan(scratch.path(), false).unwrap();
        let expected = [
            "assets/app-1a2b.js text/javascript; charset=utf-8",
            "assets/app-1a2b.js.br text/javascript; charset=utf-8",
            "assets/app-1a2b.js.gz text/javascript; charset=utf-8",
            "favicon.svg image/svg+xml",
            "fonts/plex.woff2 font/woff2",
            "index.html text/html; charset=utf-8",
        ];
        assert_eq!(listed(&unreviewed), expected);
        assert_eq!(unreviewed[5].bytes, INDEX.as_bytes());
        let reviewed = listed(&scan(scratch.path(), true).unwrap());
        assert_eq!(reviewed[6], "legal/about.json application/json");
        assert_eq!(reviewed.len(), 7);
    }

    #[test]
    fn unsafe_names_links_and_unknown_types_are_refused() {
        for (name, refusal) in [
            (".env", "unsafe browser asset name: \".env\""),
            ("assets/a b.js", "unsafe browser asset name: assets/\"a b.js\""),
            ("assets/app.exe", "unsupported browser asset type: assets/app.exe"),
            ("assets/app.exe.br", "unsupported browser asset type: assets/app.exe.br"),
        ] {
            let scratch = fixture();
            scratch.file(name, "").unwrap();
            assert_eq!(scan(scratch.path(), false).err().as_deref(), Some(refusal), "{name}");
        }
        #[cfg(unix)]
        {
            let scratch = fixture();
            std::os::unix::fs::symlink("/etc/passwd", scratch.path().join("passwd.txt")).unwrap();
            let refusal = "browser asset is a symlink or special file: passwd.txt";
            assert_eq!(scan(scratch.path(), false).err().as_deref(), Some(refusal));
        }
        let scratch = Scratch::new().unwrap();
        scratch.file("favicon.svg", "<svg/>").unwrap();
        let refusal = "the browser asset directory has no index.html";
        assert_eq!(scan(scratch.path(), false).err().as_deref(), Some(refusal));
    }
}
