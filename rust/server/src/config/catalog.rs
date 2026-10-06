//! The operator catalogue from `GM_SERVER_CATALOG` or `GM_SERVER_CATALOG_FILE` (`docs/SERVERS.md`).

use super::path_error;
use graphite_meter_proto::{
    catalog::{CatalogError, ServerCatalog, ServerEntry, ServerId},
    json,
    origin::{BaseUrl, Origin},
    text::quote,
};
use ring::digest::{SHA256, digest};
use serde::Deserialize;
use std::{fmt::Write as _, fs::File, io::Read, path::Path};

const MAX_INPUT_BYTES: usize = 64 << 10;
const MAX_NORMALIZED_BYTES: usize = 48 << 10;

/// The catalogue from whichever source is present, an empty value counting as present; with neither, `self` alone.
pub(super) fn load(inline: Option<String>, file: Option<String>) -> Result<ServerCatalog, String> {
    match (inline, file) {
        (Some(_), Some(_)) => Err("set only one of GM_SERVER_CATALOG and GM_SERVER_CATALOG_FILE".into()),
        (None, None) => Ok(ServerCatalog::singleton()),
        (Some(inline), None) => parse(inline.as_bytes()),
        (None, Some(path)) => parse(&read(&path)?),
    }
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    if !Path::new(path).is_absolute() || path.contains("..") {
        return Err("GM_SERVER_CATALOG_FILE must be an absolute path without '..'".into());
    }
    let mut data = Vec::new();
    let file =
        File::open(path).map_err(|error| format!("GM_SERVER_CATALOG_FILE: {}", path_error("open", path, &error)))?;
    file.take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|error| path_error("read", path, &error))?;
    Ok(data)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Document {
    default_selection: Option<Vec<String>>,
    #[serde(default)]
    servers: Vec<Entry>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct Entry {
    id: String,
    url: String,
    name: String,
    location: String,
    additional_origins: Vec<String>,
}

/// Parses either catalogue form, with `self` added first.
pub(super) fn parse(data: &[u8]) -> Result<ServerCatalog, String> {
    if data.len() > MAX_INPUT_BYTES {
        return Err("server catalogue exceeds 64 KiB".into());
    }
    let mut catalog = ServerCatalog::singleton();
    if data.trim_ascii_start().starts_with(b"[") {
        let origins: Vec<String> =
            serde_json::from_slice(data).map_err(|error| format!("server catalogue origins: {error}"))?;
        for text in origins {
            let origin = origin(&text).map_err(|error| format!("server catalogue origin {}: {error}", quote(&text)))?;
            catalog.servers.push(listed(&text, origin));
        }
    } else {
        let document: Document = json::decode(data).map_err(|error| format!("server catalogue: {error}"))?;
        if let Some(selection) = document.default_selection {
            catalog.default_selection = selection.iter().map(|id| server_id(id)).collect::<Result<_, _>>()?;
        }
        for entry in document.servers {
            catalog.servers.push(entry.into_server()?);
        }
    }
    let normalized = serde_json::to_vec(&catalog).map_err(|error| error.to_string())?;
    if normalized.len() > MAX_NORMALIZED_BYTES {
        return Err("normalized catalogue exceeds 48 KiB; reserve space for this server's transport origins".into());
    }
    catalog.validate().map_err(|error| error.to_string())?;
    Ok(catalog)
}

impl Entry {
    fn into_server(self) -> Result<ServerEntry, String> {
        if self.id == "self" {
            return Err("self is added automatically; omit it from servers".into());
        }
        let url = origin(&self.url).map_err(|error| format!("server {}: {error}", quote(&self.id)))?;
        let origins = self.additional_origins.iter().map(|text| origin(text));
        let additional_origins = origins.collect::<Result<_, _>>();
        Ok(ServerEntry {
            id: server_id(&self.id)?,
            url: BaseUrl::Origin(url),
            name: self.name,
            location: self.location,
            additional_origins: additional_origins.map_err(|error| error.to_string())?,
        })
    }
}

fn server_id(text: &str) -> Result<ServerId, String> {
    ServerId::parse(text).ok_or_else(|| CatalogError::Identity.to_string())
}

/// A catalogue origin, which may end in one slash.
fn origin(text: &str) -> Result<Origin, graphite_meter_proto::origin::OriginError> {
    Origin::parse(text.strip_suffix('/').unwrap_or(text))
}

/// An array-form entry: its ID hashes the Go-canonical origin, so reordering keeps IDs and Go-saved selections.
fn listed(text: &str, url: Origin) -> ServerEntry {
    let key = go_key(text.strip_suffix('/').unwrap_or(text));
    let mut id = String::from("server-");
    for byte in &digest(&SHA256, key.as_bytes()).as_ref()[..16] {
        let _ = write!(id, "{byte:02x}");
    }
    let name = key
        .split_once("://")
        .map_or_else(String::new, |(_, authority)| authority.to_owned());
    let id = ServerId::parse(&id).expect("a hashed ID is a valid server ID");
    ServerEntry {
        id,
        url: BaseUrl::Origin(url),
        name,
        location: String::new(),
        additional_origins: Vec::new(),
    }
}

/// Go's `OriginKey` of a valid ASCII origin: lower case, without an empty or default port, an IPv6 literal as written.
fn go_key(origin: &str) -> String {
    let key = origin.to_ascii_lowercase();
    let key = key.strip_suffix(':').unwrap_or(&key);
    let default = if key.starts_with("https:") { ":443" } else { ":80" };
    key.strip_suffix(default).unwrap_or(key).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(catalog: &ServerCatalog) -> Vec<&str> {
        catalog.servers.iter().map(|entry| entry.id.as_str()).collect()
    }

    #[test]
    fn array_ids_equal_the_go_servers_ids() {
        let catalog = parse(br#"["https://EXAMPLE.net:443/", "http://[2001:DB8:0:0::1]:8080", "http://h:"]"#).unwrap();
        let go_ids = [
            "server-35d5ef135871623822d3d4779a9ecac1",
            "server-e191cb5c2e7a8cc33ccdc0b44dda2e01",
            "server-4f8d848825a112f9057c104f83e580e7",
        ];
        assert_eq!(ids(&catalog)[1..], go_ids);
        let names: Vec<_> = catalog.servers[1..].iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["example.net", "[2001:db8:0:0::1]:8080", "h"]);
        let reordered = parse(br#"["http://[2001:db8:0:0::1]:8080", "https://example.net"]"#).unwrap();
        assert_eq!(ids(&reordered)[1..], [ids(&catalog)[2], ids(&catalog)[1]]);
        assert_eq!(catalog.default_selection, [ServerId::own()]);
    }

    #[test]
    fn object_form_keeps_fields_and_refuses_what_go_refuses() {
        let catalog = parse(
            br#"{"defaultSelection":["remote"],"servers":[{"id":"remote","name":"Remote",
            "url":"https://EXAMPLE.net:443","additionalOrigins":["https://transfer.example.net/"]}]}"#,
        )
        .unwrap();
        let remote = &catalog.servers[1];
        assert_eq!((catalog.servers[0].id.as_str(), remote.id.as_str()), ("self", "remote"));
        assert_eq!(remote.url, BaseUrl::Origin(Origin::parse("https://example.net").unwrap()));
        assert_eq!(remote.additional_origins, [Origin::parse("https://transfer.example.net").unwrap()]);
        for (document, message) in [
            (r#"{"servers":[{"id":"self","url":"https://a.example"}]}"#, "self is added automatically"),
            (r#"{"servers":[{"id":"a","url":"https://a.example/path"}]}"#, "server \"a\": expected"),
            (r#"{"servers":[],"extra":1}"#, "server catalogue: unknown field"),
            (r#"{"defaultSelection":[],"servers":[]}"#, "select one to four"),
            (r#"{"defaultSelection":["gone"],"servers":[]}"#, "select one to four"),
            (
                r#"{"servers":[{"id":"a b","url":"https://a.example"}]}"#,
                "invalid catalogue server identity",
            ),
            (r#"["https://a.example","https://A.example:443"]"#, "duplicate catalogue server"),
            (r#"["ftp://a.example"]"#, "server catalogue origin \"ftp://a.example\": expected"),
        ] {
            let error = parse(document.as_bytes()).unwrap_err();
            assert!(error.contains(message), "{document}: {error}");
        }
    }
}
