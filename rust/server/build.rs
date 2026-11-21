//! Embeds the third-party notices and, from `GM_ASSET_DIR`, the browser app.

#[path = "src/assets/scan.rs"]
mod scan;

use std::{env, fs, path::Path};

fn main() {
    if let Err(error) = embed() {
        panic!("server build inputs: {error}");
    }
}

fn embed() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed=GM_ASSET_DIR");
    let configured = env::var_os("GM_ASSET_DIR");
    let reviewed = graphite_meter_legal::embed(configured.is_some())?;
    let output = graphite_meter_legal::out_dir()?;
    let output = output.as_path();
    let mut manifest = String::from("static EMBEDDED: &[Asset] = &[\n");
    if let Some(configured) = configured {
        if configured.is_empty() {
            return Err("GM_ASSET_DIR must name a directory".into());
        }
        let root = graphite_meter_legal::inside(
            &graphite_meter_legal::checkout()?,
            Path::new(&configured),
        )?;
        if reviewed.as_ref().is_some_and(|reviewed| *reviewed != root) {
            return Err(
                "reviewed builds embed the browser app from GM_LEGAL_DIR/browser-assets".into(),
            );
        }
        println!("cargo:rerun-if-changed={}", root.display());
        const MAX_GZIP_INFLATE_BYTES: usize = 8 << 20;
        let scanned = scan::scan(&root, reviewed.is_some())?;
        let by_name: std::collections::HashMap<&str, &[u8]> = scanned
            .iter()
            .map(|file| (file.name.as_str(), file.bytes.as_slice()))
            .collect();
        let mut elide_plain = std::collections::HashSet::new();
        for file in &scanned {
            let Some(base) = file.name.strip_suffix(".gz") else {
                continue;
            };
            let inflated =
                graphite_meter_legal::gunzip_single_member(&file.bytes, MAX_GZIP_INFLATE_BYTES)
                    .ok_or_else(|| {
                        format!(
                            "invalid or oversized gzip browser asset (must be single-member RFC 1952 <= {MAX_GZIP_INFLATE_BYTES} B): {}",
                            file.name
                        )
                    })?;
            if let Some(plain) = by_name.get(base)
                && inflated.as_slice() != *plain
            {
                return Err(format!(
                    "gzip browser asset {} does not match sibling {base}",
                    file.name
                ));
            }
            if base != "index.html" {
                elide_plain.insert(base);
            }
        }
        for (number, file) in scanned.iter().enumerate() {
            // Skip validated uncompressed plain copies when a matching .gz copy exists (except index.html).
            if elide_plain.contains(file.name.as_str()) {
                continue;
            }
            // rustc includes this checked copy, never the source file.
            let snapshot = format!("browser-asset-{number}");
            fs::write(output.join(&snapshot), &file.bytes)
                .map_err(|error| format!("{snapshot}: {error}"))?;
            let (name, content_type) = (&file.name, file.content_type);
            manifest.push_str(&format!(
                "    Asset {{ path: {name:?}, content_type: {content_type:?}, \
                 bytes: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{snapshot}\")) }},\n"
            ));
        }
    }
    manifest.push_str("];\n");
    fs::write(output.join("assets.rs"), manifest).map_err(|error| format!("assets.rs: {error}"))
}
