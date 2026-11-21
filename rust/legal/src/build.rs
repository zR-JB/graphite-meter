//! Serves the build script. It checks the notice inputs, then writes the compressed report and `legal.rs`.

use crate::BROWSER_NOTICE;
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// Opens the report of an unreviewed development build, whose executable also carries it in plain text.
const DEVELOPMENT: &str = "UNREVIEWED DEVELOPMENT BUILD";
/// Precede the SHA-256s of a reviewed build's report and PGO profile in its executable, for release verification.
const REVIEWED: &str = "graphite-meter reviewed notices sha256:";
const PROFILED: &str = "graphite-meter pgo profile sha256:";
const MAX_REPORT_BYTES: usize = 16 << 20;
/// A reviewed legal directory's notices cover the browser build in this subdirectory.
const BROWSER_ASSETS: &str = "browser-assets";

/// Checks `GM_ENGINE_VERSION` and any reviewed notices in `GM_LEGAL_DIR`, then writes `legal.rs`; `browser` returns the covered browser build.
pub fn embed(browser: bool) -> Result<Option<PathBuf>, String> {
    for name in ["GM_ENGINE_VERSION", "GM_LEGAL_DIR", "GM_PGO_SHA256"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    embed_with(
        &|name| env::var_os(name),
        &checkout()?,
        &out_dir()?,
        browser,
    )
}

pub fn out_dir() -> Result<PathBuf, String> {
    env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "missing OUT_DIR".into())
}

/// Finds the repository checkout, two levels above the building package. Build inputs never lie outside it.
pub fn checkout() -> Result<PathBuf, String> {
    let package = env::var_os("CARGO_MANIFEST_DIR").ok_or("missing CARGO_MANIFEST_DIR")?;
    canonical(&Path::new(&package).join("../.."))
}

pub fn inside(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let resolved = canonical(path)?;
    match resolved.starts_with(root) {
        true => Ok(resolved),
        false => Err(format!(
            "{} is outside {}",
            resolved.display(),
            root.display()
        )),
    }
}

fn embed_with(
    var: &dyn Fn(&str) -> Option<OsString>,
    checkout: &Path,
    output: &Path,
    browser: bool,
) -> Result<Option<PathBuf>, String> {
    if let Some(version) = var("GM_ENGINE_VERSION") {
        let release_byte = |byte: &u8| byte.is_ascii_alphanumeric() || b".-+".contains(byte);
        if version.is_empty() || !version.as_encoded_bytes().iter().all(release_byte) {
            return Err("GM_ENGINE_VERSION must be a nonempty release identifier".into());
        }
    }
    let Some(configured) = var("GM_LEGAL_DIR") else {
        generate(output, "None", None, "")?;
        return Ok(None);
    };
    if !Path::new(&configured).is_absolute() {
        return Err("GM_LEGAL_DIR must be an absolute directory".into());
    }
    let directory = inside(checkout, Path::new(&configured))?;
    for (name, variable) in [
        ("package.txt", "CARGO_PKG_NAME"),
        ("target.txt", "TARGET"),
        ("rustc-path.txt", "RUSTC"),
    ] {
        if var(variable).map(OsString::into_encoded_bytes) != Some(read(&directory.join(name))?) {
            return Err(format!("notice {name} does not match this build"));
        }
    }
    check_inputs(checkout, &directory)?;
    let report = read(&directory.join("LEGAL.txt"))?;
    if report.is_empty() || report.len() > MAX_REPORT_BYTES || std::str::from_utf8(&report).is_err()
    {
        return Err("LEGAL.txt must be UTF-8 text of 1 byte to 16 MiB".into());
    }
    let assets = directory.join(BROWSER_ASSETS);
    let start = match browser {
        true => {
            let notice = read(&assets.join(BROWSER_NOTICE))?;
            let start = report
                .ends_with(&notice)
                .then(|| report.len() - notice.len());
            Some(start.ok_or("LEGAL.txt does not end with the browser's notice")?)
        }
        false => None,
    };
    let mut marker = match report.starts_with(DEVELOPMENT.as_bytes()) {
        true => DEVELOPMENT.to_owned(),
        false => format!(
            "{REVIEWED}{}",
            hex(read(&directory.join("LEGAL.sha256"))?, "LEGAL.sha256")?
        ),
    };
    if let Some(profile) = var("GM_PGO_SHA256") {
        marker = format!(
            "{marker} {PROFILED}{}",
            hex(profile.into_encoded_bytes(), "GM_PGO_SHA256")?
        );
    }
    let (compressed, browser_spec) = match start {
        Some(offset) => {
            let mut combined = gzip_member(&report[..offset]);
            let browser_gz_offset = combined.len();
            combined.extend_from_slice(&gzip_member(&report[offset..]));
            (combined, Some((browser_gz_offset, report.len() - offset)))
        }
        None => (gzip_member(&report), None),
    };
    write(&output.join("legal.gz"), &compressed)?;
    let payload = format!(
        "Some((include_bytes!(concat!(env!(\"OUT_DIR\"), \"/legal.gz\")), {}))",
        report.len()
    );
    generate(output, &payload, browser_spec, &marker)?;
    browser.then(|| canonical(&assets)).transpose()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Deterministic RFC 1952 gzip member (mtime = 0, OS = unknown) for a notice slice.
pub(crate) fn gzip_member(plain: &[u8]) -> Vec<u8> {
    let raw = miniz_oxide::deflate::compress_to_vec(plain, 9);
    let mut out = Vec::with_capacity(10 + raw.len() + 8);
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x02, 0xff]);
    out.extend_from_slice(&raw);
    out.extend_from_slice(&crc32(plain).to_le_bytes());
    out.extend_from_slice(&(plain.len() as u32).to_le_bytes());
    out
}

/// Checks that every input the notices were prepared from still matches its snapshot.
fn check_inputs(checkout: &Path, directory: &Path) -> Result<(), String> {
    let inputs = String::from_utf8(read(&directory.join("inputs.txt"))?)
        .map_err(|_| "inputs.txt is not UTF-8")?;
    if !inputs.lines().any(|line| line == "rust/Cargo.lock") {
        return Err("inputs.txt does not name rust/Cargo.lock".into());
    }
    let snapshots = canonical(&directory.join("inputs"))?;
    for relative in inputs.lines() {
        let source = inside(checkout, &checkout.join(relative))?;
        let snapshot = inside(&snapshots, &snapshots.join(relative))?;
        if read(&source)? != read(&snapshot)? {
            return Err(format!("Rust legal notices are stale: {relative}"));
        }
    }
    Ok(())
}

/// A SHA-256 the collector wrote for the executable to carry: `LEGAL.sha256` or `GM_PGO_SHA256`.
fn hex(digest: Vec<u8>, name: &str) -> Result<String, String> {
    let digest = String::from_utf8(digest).unwrap_or_default();
    match digest.len() == 64
        && digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        true => Ok(digest),
        false => Err(format!("{name} must hold 64 lowercase hexadecimal digits")),
    }
}

fn generate(
    output: &Path,
    payload: &str,
    browser: Option<(usize, usize)>,
    marker: &str,
) -> Result<(), String> {
    let source = format!(
        "pub static NOTICES: graphite_meter_legal::Notices = \
         graphite_meter_legal::Notices::new({payload}, {browser:?}, {marker:?});\n"
    );
    write(&output.join("legal.rs"), source.as_bytes())
}

/// Reads `path`, which Cargo then watches.
fn read(path: &Path) -> Result<Vec<u8>, String> {
    println!("cargo:rerun-if-changed={}", path.display());
    fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_testkit::Scratch;

    const REPORT: &str = "UNREVIEWED DEVELOPMENT BUILD\n\nproject\nrust crates\nbrowser notice\n";
    /// The collector writes this digest file, which only a reviewed report's marker carries.
    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Builds a checkout holding a reviewed legal directory for the server's x86_64 musl build.
    fn checkout() -> (Scratch, PathBuf) {
        let scratch = Scratch::new().unwrap();
        for (name, contents) in [
            ("rust/Cargo.lock", "lock"),
            ("legal/package.txt", "graphite-meter-server"),
            ("legal/target.txt", "x86_64-unknown-linux-musl"),
            ("legal/rustc-path.txt", "/toolchain/rustc"),
            ("legal/inputs.txt", "rust/Cargo.lock\n"),
            ("legal/inputs/rust/Cargo.lock", "lock"),
            ("legal/LEGAL.txt", REPORT),
            ("legal/LEGAL.sha256", DIGEST),
            (
                "legal/browser-assets/legal/THIRD_PARTY_NOTICES.txt",
                "browser notice\n",
            ),
        ] {
            scratch.file(name, contents).unwrap();
        }
        scratch.dir("out").unwrap();
        let root = canonical(scratch.path()).unwrap();
        (scratch, root)
    }

    fn run(
        root: &Path,
        overrides: &[(&str, &str)],
        browser: bool,
    ) -> Result<Option<PathBuf>, String> {
        let legal = root.join("legal").display().to_string();
        let defaults = [
            ("GM_LEGAL_DIR", legal.as_str()),
            ("CARGO_PKG_NAME", "graphite-meter-server"),
            ("TARGET", "x86_64-unknown-linux-musl"),
            ("RUSTC", "/toolchain/rustc"),
        ];
        let var = |name: &str| {
            Some(
                overrides
                    .iter()
                    .chain(&defaults)
                    .find(|(key, _)| *key == name)?
                    .1
                    .into(),
            )
        };
        embed_with(&var, root, &root.join("out"), browser)
    }

    fn generated(root: &Path) -> String {
        fs::read_to_string(root.join("out/legal.rs")).unwrap()
    }

    #[test]
    fn a_build_without_notices_embeds_none() {
        let (_scratch, root) = checkout();
        assert_eq!(
            embed_with(&|_| None, &root, &root.join("out"), true),
            Ok(None)
        );
        assert!(generated(&root).ends_with("Notices::new(None, None, \"\");\n"));
    }

    #[test]
    fn reviewed_notices_embed_the_compressed_report_and_the_browser_offset() {
        let (_scratch, root) = checkout();
        assert_eq!(
            run(&root, &[], true),
            Ok(Some(root.join("legal/browser-assets")))
        );
        let compressed = fs::read(root.join("out/legal.gz")).unwrap();
        let browser_len = "browser notice\n".len();
        let prefix_len = REPORT.len() - browser_len;
        let browser_gz_offset = gzip_member(&REPORT.as_bytes()[..prefix_len]).len();
        let expected = format!(
            "/legal.gz\")), {})), Some(({browser_gz_offset}, {browser_len})), \"UNREVIEWED DEVELOPMENT BUILD\");\n",
            REPORT.len()
        );
        assert!(
            generated(&root).ends_with(&expected),
            "{}",
            generated(&root)
        );
        let static_gz: &'static [u8] = Box::leak(compressed.into_boxed_slice());
        let notices = crate::Notices::new(
            Some((static_gz, REPORT.len())),
            Some((browser_gz_offset, browser_len)),
            "",
        );
        assert_eq!(notices.report().unwrap(), REPORT.as_bytes());
        assert_eq!(notices.browser().unwrap(), b"browser notice\n");
        assert_eq!(
            notices.browser_gzip().unwrap(),
            gzip_member(b"browser notice\n").as_slice()
        );
        assert_eq!(run(&root, &[], false), Ok(None));
        assert!(generated(&root).ends_with(&format!(
            "{})), None, \"UNREVIEWED DEVELOPMENT BUILD\");\n",
            REPORT.len()
        )));
    }

    #[test]
    fn a_reviewed_report_s_marker_carries_its_digest() {
        let (scratch, root) = checkout();
        scratch.file("legal/LEGAL.txt", "project\n").unwrap();
        assert_eq!(run(&root, &[], false), Ok(None));
        let marker = format!(", None, \"graphite-meter reviewed notices sha256:{DIGEST}\");\n");
        assert!(generated(&root).ends_with(&marker), "{}", generated(&root));
        let refusal = "LEGAL.sha256 must hold 64 lowercase hexadecimal digits";
        for digest in [&DIGEST[1..], &DIGEST.to_uppercase(), &format!("{DIGEST}\n")] {
            scratch.file("legal/LEGAL.sha256", digest).unwrap();
            assert_eq!(run(&root, &[], false), Err(refusal.into()));
        }
    }

    #[test]
    fn a_profiled_build_s_marker_carries_its_profile_s_digest() {
        let (scratch, root) = checkout();
        scratch.file("legal/LEGAL.txt", "project\n").unwrap();
        let profile = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        assert_eq!(run(&root, &[("GM_PGO_SHA256", profile)], false), Ok(None));
        let marker = format!(
            ", None, \"graphite-meter reviewed notices sha256:{DIGEST} graphite-meter pgo profile sha256:{profile}\");\n"
        );
        assert!(generated(&root).ends_with(&marker), "{}", generated(&root));
        assert_eq!(
            run(&root, &[("GM_PGO_SHA256", &profile[1..])], false),
            Err("GM_PGO_SHA256 must hold 64 lowercase hexadecimal digits".into())
        );
    }

    #[test]
    fn inputs_that_do_not_match_this_build_are_refused() {
        let (scratch, root) = checkout();
        let refusal = |overrides: &[(&str, &str)]| run(&root, overrides, true).unwrap_err();
        assert_eq!(
            refusal(&[("CARGO_PKG_NAME", "graphite-meter-client")]),
            "notice package.txt does not match this build"
        );
        assert_eq!(
            refusal(&[("TARGET", "aarch64-unknown-linux-musl")]),
            "notice target.txt does not match this build"
        );
        assert_eq!(
            refusal(&[("RUSTC", "rustc")]),
            "notice rustc-path.txt does not match this build"
        );
        assert_eq!(
            refusal(&[("GM_LEGAL_DIR", "legal")]),
            "GM_LEGAL_DIR must be an absolute directory"
        );
        let outside = root.parent().unwrap().display().to_string();
        assert_eq!(
            refusal(&[("GM_LEGAL_DIR", &outside)]),
            format!("{outside} is outside {}", root.display())
        );
        let version = "GM_ENGINE_VERSION must be a nonempty release identifier";
        assert_eq!(refusal(&[("GM_ENGINE_VERSION", "")]), version);
        assert_eq!(refusal(&[("GM_ENGINE_VERSION", "1.0 beta")]), version);
        assert_eq!(
            run(&root, &[("GM_ENGINE_VERSION", "1.2.0-rc.1+rust")], false),
            Ok(None)
        );
        scratch
            .file(
                "legal/browser-assets/legal/THIRD_PARTY_NOTICES.txt",
                "other\n",
            )
            .unwrap();
        assert_eq!(
            refusal(&[]),
            "LEGAL.txt does not end with the browser's notice"
        );
        scratch.file("rust/Cargo.lock", "changed").unwrap();
        assert_eq!(
            refusal(&[]),
            "Rust legal notices are stale: rust/Cargo.lock"
        );
        scratch
            .file("legal/inputs.txt", "rust/Cargo.toml\n")
            .unwrap();
        assert_eq!(refusal(&[]), "inputs.txt does not name rust/Cargo.lock");
    }
}
