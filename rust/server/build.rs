#![forbid(unsafe_code)]

#[path = "../legal_build.rs"]
mod legal;

use std::{
    env,
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    if let Err(error) = legal::embed() {
        panic!("Rust legal notice embedding failed: {error}");
    }
    println!("cargo:rerun-if-env-changed=GM_RUST_ASSET_DIR");
    if let Err(error) = generate() {
        panic!("browser asset embedding failed: {error}");
    }
}

// Asset directories are trusted build inputs. Symlink checks prevent accidental
// traversal; they do not defend against concurrent filesystem replacement.
fn generate() -> Result<()> {
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    let Some(configured) = env::var_os("GM_RUST_ASSET_DIR") else {
        fs::write(
            output.join("browser_assets.rs"),
            "static EMBEDDED: &[EmbeddedAsset] = &[];\n",
        )?;
        return Ok(());
    };
    if configured.is_empty() {
        return Err("GM_RUST_ASSET_DIR must not be empty".into());
    }
    let configured = PathBuf::from(configured);
    if configured
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("GM_RUST_ASSET_DIR must not contain '..'".into());
    }
    // Relative asset directories always resolve against this package, not cwd.
    let root = if configured.is_absolute() {
        configured
    } else {
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing package directory")?)
            .join(configured)
    };
    let reviewed_legal = if let Some(directory) = env::var_os("GM_RUST_LEGAL_DIR") {
        let expected = PathBuf::from(directory).join("browser-assets");
        if root != expected {
            return Err(
                "reviewed Rust legal assets must come from GM_RUST_LEGAL_DIR/browser-assets".into(),
            );
        }
        true
    } else {
        false
    };
    let root_text = root.to_str().ok_or("asset directory must be UTF-8")?;
    if root_text.chars().any(char::is_control) {
        return Err("asset directory must not contain control characters".into());
    }
    reject_symlink_components(&root)?;
    if !fs::metadata(&root)?.is_dir() {
        return Err("GM_RUST_ASSET_DIR must name a directory".into());
    }
    let mut files = Vec::new();
    collect(&root, "", &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut manifest = String::from("static EMBEDDED: &[EmbeddedAsset] = &[\n");
    let mut index_found = false;
    for (number, (name, source)) in files.into_iter().enumerate() {
        // The ordinary browser build contains Go notices. Never embed them in
        // an unreviewed Rust binary as if they described its dependency closure.
        if name.starts_with("legal/") && !reviewed_legal {
            continue;
        }
        let content_type = content_type(&name)
            .ok_or_else(|| format!("unsupported browser asset extension: {name}"))?;
        let bytes = fs::read(&source)?;
        if name == "index.html" {
            let html = std::str::from_utf8(&bytes)?;
            if html.matches("</head>").count() != 1 {
                return Err("index.html must contain exactly one closing head element".into());
            }
            if html.matches("<script>").count() > 1 {
                return Err(
                    "index.html has multiple inline scripts; CSP supports one pre-paint script"
                        .into(),
                );
            }
            if let Some((_, script)) = html.split_once("<script>")
                && !script.contains("</script>")
            {
                return Err("index.html has an unterminated inline script".into());
            }
            if html.contains("name=\"graphite-meter-auth\"")
                || html.contains("name=\"graphite-meter-result-history-default\"")
            {
                return Err(
                    "index.html must not contain server-owned auth/history metadata".into(),
                );
            }
            index_found = true;
        }
        // Embed the checked snapshot, not a source path reopened later by rustc.
        let snapshot = format!("browser-asset-{number}.bin");
        fs::write(output.join(&snapshot), bytes)?;
        manifest.push_str(&format!("    EmbeddedAsset {{ path: {name:?}, content_type: {content_type:?}, bytes: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{snapshot}\")) }},\n"));
    }
    if !index_found {
        return Err("browser asset directory has no index.html".into());
    }
    manifest.push_str("];\n");
    fs::write(output.join("browser_assets.rs"), manifest)?;
    Ok(())
}

fn reject_symlink_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(format!("symlink asset path is forbidden: {}", current.display()).into());
        }
    }
    Ok(())
}

fn collect(directory: &Path, prefix: &str, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
    println!("cargo:rerun-if-changed={}", directory.display());
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "asset names must be UTF-8")?;
        if name.starts_with('.')
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(format!("unsafe browser asset name: {name:?}").into());
        }
        let relative = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(format!("symlink asset is forbidden: {relative}").into());
        }
        if kind.is_dir() {
            collect(&entry.path(), &format!("{relative}/"), files)?;
        } else if kind.is_file() {
            files.push((relative, entry.path()));
        } else {
            return Err(format!("asset is not a regular file: {relative}").into());
        }
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
