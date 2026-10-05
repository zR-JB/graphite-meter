//! Embeds the third-party notices and, from `GM_RUST_ASSET_DIR`, the browser app.

#[path = "src/assets/scan.rs"]
mod scan;

use std::{env, fs, path::Path};

fn main() {
    if let Err(error) = embed() {
        panic!("server build inputs: {error}");
    }
}

fn embed() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed=GM_RUST_ASSET_DIR");
    let configured = env::var_os("GM_RUST_ASSET_DIR");
    let reviewed = graphite_meter_legal::embed(configured.is_some())?;
    let output = env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?;
    let output = Path::new(&output);
    let mut manifest = String::from("static EMBEDDED: &[Asset] = &[\n");
    if let Some(configured) = configured {
        if configured.is_empty() {
            return Err("GM_RUST_ASSET_DIR must name a directory".into());
        }
        // A relative directory resolves against this package.
        let root = graphite_meter_legal::inside(&graphite_meter_legal::checkout()?, Path::new(&configured))?;
        if reviewed.as_ref().is_some_and(|reviewed| *reviewed != root) {
            return Err("reviewed builds embed the browser app from GM_RUST_LEGAL_DIR/browser-assets".into());
        }
        println!("cargo:rerun-if-changed={}", root.display());
        for (number, file) in scan::scan(&root, reviewed.is_some())?.into_iter().enumerate() {
            // rustc includes this checked copy, never the source file.
            let snapshot = format!("browser-asset-{number}");
            fs::write(output.join(&snapshot), &file.bytes).map_err(|error| format!("{snapshot}: {error}"))?;
            let (name, content_type) = (file.name, file.content_type);
            manifest.push_str(&format!(
                "    Asset {{ path: {name:?}, content_type: {content_type:?}, \
                 bytes: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{snapshot}\")) }},\n"
            ));
        }
    }
    manifest.push_str("];\n");
    fs::write(output.join("assets.rs"), manifest).map_err(|error| format!("assets.rs: {error}"))
}
