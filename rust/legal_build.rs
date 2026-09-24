use std::{env, error::Error, fs, path::PathBuf, process::Command};

pub fn embed(share_browser_notices: bool) -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=GM_ENGINE_VERSION");
    if let Ok(version) = env::var("GM_ENGINE_VERSION") {
        if version.is_empty()
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
        {
            return Err("GM_ENGINE_VERSION must be a nonempty release identifier".into());
        }
        println!("cargo:rustc-env=GM_ENGINE_VERSION={version}");
    }

    println!("cargo:rerun-if-env-changed=GM_RUST_LEGAL_DIR");
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    let identity = build_identity();
    fs::write(output.join("legal-build-identity.txt"), &identity)?;
    let Some(configured) = env::var_os("GM_RUST_LEGAL_DIR") else {
        fs::write(
            output.join("legal.rs"),
            if share_browser_notices {
                "const LEGAL: Option<&str> = None;\nconst LEGAL_USES_BROWSER_NOTICES: bool = false;\n"
            } else {
                "const LEGAL: Option<&str> = None;\n"
            },
        )?;
        return Ok(());
    };
    let directory = PathBuf::from(configured);
    if !directory.is_absolute() {
        return Err("GM_RUST_LEGAL_DIR must be an absolute directory".into());
    }
    let identity_path = directory.join("build-identity.txt");
    println!("cargo:rerun-if-changed={}", identity_path.display());
    if fs::read_to_string(identity_path)? != identity {
        return Err(
            "notice features, compiler flags, or build profile do not match this build".into(),
        );
    }
    let package =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest directory")?);
    let repo = package
        .parent()
        .and_then(|path| path.parent())
        .ok_or("missing repository root")?;
    for (name, expected) in [
        ("package.txt", env::var("CARGO_PKG_NAME")?),
        ("target.txt", env::var("TARGET")?),
    ] {
        let path = directory.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        if fs::read_to_string(path)? != expected {
            return Err(format!("notice {name} does not match this build").into());
        }
    }
    let compiler = Command::new(env::var_os("RUSTC").ok_or("missing RUSTC")?)
        .arg("-vV")
        .output()?;
    let compiler_path = directory.join("rustc.txt");
    println!("cargo:rerun-if-changed={}", compiler_path.display());
    if !compiler.status.success() || fs::read(compiler_path)? != compiler.stdout {
        return Err("notice compiler identity does not match this build".into());
    }
    let index = directory.join("inputs.txt");
    println!("cargo:rerun-if-changed={}", index.display());
    let inputs = fs::read_to_string(index)?;
    if !inputs.lines().any(|line| line == "rust/Cargo.lock") {
        return Err("notice input manifest has no Cargo.lock".into());
    }
    for relative in inputs.lines() {
        let path = std::path::Path::new(relative);
        if !path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err("notice input path must be relative and normalized".into());
        }
        let source = repo.join(path);
        let snapshot = directory.join("inputs").join(path);
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
    // The release server already embeds the exact reviewed browser notice as
    // an asset. Reuse that one static byte slice for --legal instead of
    // embedding a second multi-megabyte copy in the executable.
    let shared = share_browser_notices && env::var_os("GM_RUST_ASSET_DIR").is_some();
    let (name, legal) = if shared {
        let notices_path = directory.join("browser-assets/legal/THIRD_PARTY_NOTICES.txt");
        println!("cargo:rerun-if-changed={}", notices_path.display());
        let notices = fs::read_to_string(notices_path)?;
        let prefix = text
            .strip_suffix(&notices)
            .ok_or("reviewed CLI notice does not end with the browser notice")?;
        ("LEGAL_PREFIX.txt", prefix)
    } else {
        ("LEGAL.txt", text.as_str())
    };
    // Copy the checked build input so rustc does not reopen a mutable source file.
    fs::write(output.join(name), legal)?;
    let mut generated = format!(
        "const LEGAL: Option<&str> = Some(include_str!(concat!(env!(\"OUT_DIR\"), \"/{name}\")));\n"
    );
    if share_browser_notices {
        generated.push_str(&format!(
            "const LEGAL_USES_BROWSER_NOTICES: bool = {shared};\n"
        ));
    }
    fs::write(output.join("legal.rs"), generated)?;
    Ok(())
}

fn build_identity() -> String {
    let mut values: Vec<_> = env::vars()
        .filter(|(name, _)| {
            name.starts_with("CARGO_FEATURE_")
                || matches!(
                    name.as_str(),
                    "TARGET"
                        | "PROFILE"
                        | "DEBUG"
                        | "OPT_LEVEL"
                        | "CARGO_ENCODED_RUSTFLAGS"
                        | "CARGO_CFG_TARGET_FEATURE"
                )
        })
        .collect();
    values.sort();
    // Debug escaping keeps embedded line breaks and separators unambiguous.
    format!("{values:?}")
}
