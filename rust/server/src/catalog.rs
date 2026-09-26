//! Bounded server catalogue configuration decoding.

use graphite_meter_core::{
    catalog::{ServerCatalog, ServerEntry},
    origin::{canonical_origin, target_origin},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{error::Error, fs::File, io::Read};

const MAX_INPUT_BYTES: usize = 64 << 10;
const MAX_NORMALIZED_BYTES: usize = 48 << 10;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct RawCatalog {
    default_selection: Option<Vec<Option<String>>>,
    servers: Option<Vec<Option<RawEntry>>>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct RawEntry {
    id: Option<String>,
    url: Option<String>,
    name: Option<String>,
    location: Option<String>,
    additional_origins: Option<Vec<Option<String>>>,
}

/// Source presence is significant, including empty strings. No process environment is read.
pub fn load(inline: Option<&str>, file: Option<&str>) -> Result<ServerCatalog> {
    match (inline, file) {
        (Some(_), Some(_)) => {
            Err("set only one of GM_SERVER_CATALOG and GM_SERVER_CATALOG_FILE".into())
        }
        (None, None) => Ok(ServerCatalog::singleton()),
        (Some(raw), None) => parse(raw.as_bytes()),
        (None, Some(path)) => {
            let mut data = Vec::new();
            File::open(path)?
                .take((MAX_INPUT_BYTES + 1) as u64)
                .read_to_end(&mut data)?;
            parse(&data)
        }
    }
}

fn canonical(raw: &str) -> Result<String> {
    Ok(canonical_origin(raw.strip_suffix('/').unwrap_or(raw))?)
}

pub fn parse(data: &[u8]) -> Result<ServerCatalog> {
    if data.len() > MAX_INPUT_BYTES {
        return Err("server catalogue exceeds 64 KiB".into());
    }
    let mut catalog = ServerCatalog::singleton();
    if data.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
        let origins: Vec<Option<String>> = serde_json::from_slice(data)?;
        for raw in origins {
            let url = canonical(&raw.unwrap_or_default())?;
            let origin = target_origin(&url)?.ok_or("expected absolute origin")?;
            let digest = Sha256::digest(url.as_bytes());
            let mut id = String::from("server-");
            use std::fmt::Write;
            for byte in &digest[..16] {
                write!(&mut id, "{byte:02x}")?;
            }
            catalog.servers.push(ServerEntry {
                id,
                url,
                name: origin.authority(),
                ..ServerEntry::default()
            });
        }
    } else {
        // Go accepts a root null as an empty catalogue, then inserts self.
        let raw: Option<RawCatalog> = serde_json::from_slice(data)?;
        let raw = raw.unwrap_or_default();
        if let Some(selected) = raw.default_selection {
            catalog.default_selection = selected
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect();
        }
        for entry in raw.servers.unwrap_or_default() {
            let entry = entry.unwrap_or_default();
            let id = entry.id.unwrap_or_default();
            if id == "self" {
                return Err("self is added automatically; omit it from servers".into());
            }
            let url = canonical(&entry.url.unwrap_or_default())?;
            let additional_origins = entry
                .additional_origins
                .unwrap_or_default()
                .into_iter()
                .map(|raw| canonical(&raw.unwrap_or_default()))
                .collect::<Result<Vec<_>>>()?;
            catalog.servers.push(ServerEntry {
                id,
                url,
                name: entry.name.unwrap_or_default(),
                location: entry.location.unwrap_or_default(),
                additional_origins,
            });
        }
    }
    if serde_json::to_vec(&catalog)?.len() > MAX_NORMALIZED_BYTES {
        return Err("normalized catalogue exceeds 48 KiB; reserve space for this server's transport origins".into());
    }
    catalog.validate()?;
    Ok(catalog)
}
