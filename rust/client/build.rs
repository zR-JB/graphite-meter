#![forbid(unsafe_code)]

#[path = "../legal_build.rs"]
mod legal;

fn main() {
    if let Err(error) = legal::embed(false) {
        panic!("Rust legal notice embedding failed: {error}");
    }
    if let Err(error) = compress_notices() {
        panic!("Rust legal notice compression failed: {error}");
    }
}

fn compress_notices() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, fs};

    let output = legal::output_directory(&legal::checkout()?)?;
    if env::var_os("GM_RUST_LEGAL_DIR").is_none() {
        fs::write(
            output.join("legal.rs"),
            "const LEGAL: Option<(&[u8], usize)> = None;\n",
        )?;
        return Ok(());
    };
    let report = fs::read(output.join("LEGAL.txt"))?;
    const MAX_LEGAL_BYTES: usize = 16 * 1024 * 1024;
    if report.len() > MAX_LEGAL_BYTES {
        return Err("reviewed legal report exceeds 16 MiB".into());
    }
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&report, 9);
    fs::write(output.join("LEGAL.zlib"), compressed)?;
    fs::write(
        output.join("legal.rs"),
        format!(
            "const LEGAL: Option<(&[u8], usize)> = Some((include_bytes!(concat!(env!(\"OUT_DIR\"), \"/LEGAL.zlib\")), {}));\n",
            report.len()
        ),
    )?;
    Ok(())
}
