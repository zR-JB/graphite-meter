use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Opens the notices of an unreviewed development build (scripts/legal/rust.py); its executable also carries
/// it in plain text, beside the compressed notices, for release verification to find.
const DEVELOPMENT: &str = "UNREVIEWED DEVELOPMENT BUILD";

/// Cargo runs build scripts in their package directory, two levels below the checkout.
pub fn checkout() -> Result<PathBuf> {
    Ok(fs::canonicalize("../..")?)
}

/// Resolves `path` and requires it inside `root`. Build inputs, reviewed notices and
/// Cargo's target directory all live in the checkout; build scripts touch nothing else.
pub fn inside(root: &Path, path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    match fs::canonicalize(path) {
        Ok(resolved) if resolved.starts_with(root) => Ok(resolved),
        Ok(resolved) => Err(format!("{} is outside {}", resolved.display(), root.display()).into()),
        Err(error) => Err(format!("{}: {error}", path.display()).into()),
    }
}

pub fn output_directory(repo: &Path) -> Result<PathBuf> {
    match env::var_os("OUT_DIR") {
        Some(output) => inside(repo, output),
        None => Err("missing OUT_DIR".into()),
    }
}

pub fn embed(share_browser_notices: bool) -> Result<()> {
    println!("cargo:rerun-if-env-changed=GM_ENGINE_VERSION");
    if let Ok(version) = env::var("GM_ENGINE_VERSION") {
        if version.is_empty()
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
        {
            return Err("GM_ENGINE_VERSION must be a nonempty release identifier".into());
        }
        let version: String = version.bytes().map(char::from).collect();
        println!("cargo:rustc-env=GM_ENGINE_VERSION={version}");
    }

    println!("cargo:rerun-if-env-changed=GM_RUST_LEGAL_DIR");
    let repo = checkout()?;
    let output = output_directory(&repo)?;
    let Some(configured) = env::var_os("GM_RUST_LEGAL_DIR") else {
        return write_constants(&output, share_browser_notices, None, None, false);
    };
    if !Path::new(&configured).is_absolute() {
        return Err("GM_RUST_LEGAL_DIR must be an absolute directory".into());
    }
    let directory = inside(&repo, configured)?;
    for (name, expected) in [("package.txt", env::var("CARGO_PKG_NAME")?), ("target.txt", env::var("TARGET")?)] {
        let path = directory.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        if fs::read_to_string(path)? != expected {
            return Err(format!("notice {name} does not match this build").into());
        }
    }
    // The notices describe the pinned toolchain's sysroot; Cargo must build with that compiler.
    let compiler_path = directory.join("rustc-path.txt");
    println!("cargo:rerun-if-changed={}", compiler_path.display());
    if env::var_os("RUSTC") != Some(fs::read_to_string(compiler_path)?.into()) {
        return Err("notice compiler does not match this build".into());
    }
    let index = directory.join("inputs.txt");
    println!("cargo:rerun-if-changed={}", index.display());
    let inputs = fs::read_to_string(index)?;
    if !inputs.lines().any(|line| line == "rust/Cargo.lock") {
        return Err("notice input manifest has no Cargo.lock".into());
    }
    let snapshots = directory.join("inputs");
    for relative in inputs.lines() {
        let source = inside(&repo, repo.join(relative))?;
        let snapshot = inside(&snapshots, snapshots.join(relative))?;
        println!("cargo:rerun-if-changed={}", source.display());
        println!("cargo:rerun-if-changed={}", snapshot.display());
        if fs::read(source)? != fs::read(snapshot)? {
            return Err(format!("Rust legal notices are stale: {relative}").into());
        }
    }
    let report = directory.join("LEGAL.txt");
    println!("cargo:rerun-if-changed={}", report.display());
    let text = fs::read_to_string(report)?;
    if text.is_empty() {
        return Err("empty Rust legal report".into());
    }
    if text.len() > 16 * 1024 * 1024 {
        return Err("reviewed legal report exceeds 16 MiB".into());
    }
    // The release server serves its reviewed browser notice, the report's suffix, from the report.
    let notices = if share_browser_notices && env::var_os("GM_RUST_ASSET_DIR").is_some() {
        let notices_path = directory.join("browser-assets/legal/THIRD_PARTY_NOTICES.txt");
        println!("cargo:rerun-if-changed={}", notices_path.display());
        let notices = fs::read_to_string(notices_path)?;
        let prefix = text
            .strip_suffix(&notices)
            .ok_or("reviewed CLI notice does not end with the browser notice")?;
        Some(prefix.len())
    } else {
        None
    };
    // Compress the checked build input so rustc does not reopen a mutable source file.
    fs::write(output.join("LEGAL.zlib"), miniz_oxide::deflate::compress_to_vec_zlib(text.as_bytes(), 9))?;
    let development = text.starts_with(DEVELOPMENT);
    write_constants(&output, share_browser_notices, Some(text.len()), notices, development)
}

fn write_constants(
    output: &Path,
    share_browser_notices: bool,
    report: Option<usize>,
    notices: Option<usize>,
    development: bool,
) -> Result<()> {
    let mut generated = match report {
        Some(length) => format!(
            "const LEGAL: Option<(&[u8], usize)> = Some((include_bytes!(concat!(env!(\"OUT_DIR\"), \"/LEGAL.zlib\")), {length}));\n"
        ),
        None => "const LEGAL: Option<(&[u8], usize)> = None;\n".into(),
    };
    // Each main.rs refers to the marker, so that no linker drops it.
    let marker = if development { DEVELOPMENT } else { "" };
    generated.push_str(&format!("#[used]\npub static DEVELOPMENT_NOTICES: &str = {marker:?};\n"));
    if share_browser_notices {
        generated.push_str(&format!("const LEGAL_NOTICES: Option<usize> = {notices:?};\n"));
    }
    fs::write(output.join("legal.rs"), generated)?;
    Ok(())
}
