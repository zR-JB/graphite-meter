//! Public measurement authorities and deterministic discovery boundaries.

use crate::{
    discovery::{DiscoveryError, Preflight, null_default},
    origin::{browser_connect_source_supported, canonical_origin, catalog_origin, key, target_origin},
    text::label,
};
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_CATALOG_SERVERS: usize = 32;
pub const MAX_SELECTED_SERVERS: usize = 4;

/// Go reads a JSON null as the zero value, a list's elements included.
fn null_list<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(deserializer: D) -> Result<Vec<T>, D::Error> {
    let list = Option::<Vec<Option<T>>>::deserialize(deserializer)?.unwrap_or_default();
    Ok(list.into_iter().map(Option::unwrap_or_default).collect())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerEntry {
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(deserialize_with = "null_default")]
    pub url: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default", skip_serializing_if = "String::is_empty")]
    pub location: String,
    #[serde(deserialize_with = "null_list", skip_serializing_if = "Vec::is_empty")]
    pub additional_origins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCatalog {
    #[serde(default, deserialize_with = "null_list")]
    pub default_selection: Vec<String>,
    #[serde(default, deserialize_with = "null_list")]
    pub servers: Vec<ServerEntry>,
}

errors! {
    pub enum CatalogError {
        InvalidServers => "catalogue requires self followed by at most 31 additional servers",
        InvalidIdentity => "invalid catalogue server identity",
        DuplicateServer => "duplicate catalogue server",
        InvalidOrigin => "invalid catalogue origin",
        TooManyAdditionalOrigins => "too many additional origins",
        InvalidSelection => "select one to four distinct known servers",
    }
}

/// A received catalogue entry left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub id: String,
    pub error: CatalogError,
}

impl Default for ServerCatalog {
    fn default() -> Self {
        Self::singleton()
    }
}

impl ServerCatalog {
    pub fn singleton() -> Self {
        Self {
            default_selection: vec!["self".into()],
            servers: vec![ServerEntry {
                id: "self".into(),
                url: ".".into(),
                name: "graphite-meter".into(),
                ..ServerEntry::default()
            }],
        }
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        self.validate_servers()?;
        for index in 0..self.servers.len() {
            self.validate_entry(index)?;
        }
        self.validate_selection(&self.default_selection)
    }

    /// Self first, and at most 31 servers after it.
    fn validate_servers(&self) -> Result<(), CatalogError> {
        match self.servers.first() {
            Some(first) if first.id == "self" && self.servers.len() <= MAX_CATALOG_SERVERS => Ok(()),
            _ => Err(CatalogError::InvalidServers),
        }
    }

    /// One entry, against the entries before it.
    fn validate_entry(&self, index: usize) -> Result<(), CatalogError> {
        let entry = &self.servers[index];
        let id_byte = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        let valid_id = !entry.id.is_empty() && entry.id.len() <= 64 && entry.id.bytes().all(id_byte);
        if !valid_id || !label(&entry.name) || !label(&entry.location) {
            return Err(CatalogError::InvalidIdentity);
        }
        let origin_key = key(&entry.url);
        let duplicate = |prior: &ServerEntry| prior.id == entry.id || key(&prior.url) == origin_key;
        if self.servers[..index].iter().any(duplicate) {
            return Err(CatalogError::DuplicateServer);
        }
        let invalid = |raw: &String| canonical_origin(raw).is_err();
        let url_valid = if entry.url == "." { entry.id == "self" } else { !invalid(&entry.url) };
        if !url_valid {
            return Err(CatalogError::InvalidOrigin);
        }
        if entry.additional_origins.len() > 32 {
            return Err(CatalogError::TooManyAdditionalOrigins);
        }
        if entry.additional_origins.iter().any(invalid) {
            return Err(CatalogError::InvalidOrigin);
        }
        Ok(())
    }

    /// A catalogue as a client receives it. Its origins may end in one slash and name
    /// international hosts, as Go's catalogues may (`catalog_origin`); an entry still invalid
    /// is left out alone and returned with its reason, and the default selection drops it, or
    /// falls back to self if nothing else is left. The catalogue as a whole must still hold.
    pub fn received(mut self) -> Result<(Self, Vec<Rejected>), CatalogError> {
        self.validate_servers()?;
        let mut rejected = Vec::new();
        let servers = std::mem::take(&mut self.servers);
        for mut entry in servers {
            if entry.url != "." {
                entry.url = catalog_origin(&entry.url).unwrap_or(entry.url);
            }
            for raw in &mut entry.additional_origins {
                if let Ok(origin) = catalog_origin(raw) {
                    *raw = origin;
                }
            }
            self.servers.push(entry);
            let index = self.servers.len() - 1;
            if let Err(error) = self.validate_entry(index) {
                if index == 0 {
                    return Err(error);
                }
                let entry = self.servers.pop().expect("the entry just pushed");
                rejected.push(Rejected { id: entry.id, error });
            }
        }
        let servers = &self.servers;
        self.default_selection
            .retain(|id| servers.iter().any(|entry| entry.id == *id));
        if self.default_selection.is_empty() {
            self.default_selection.push("self".into());
        }
        self.validate_selection(&self.default_selection)?;
        Ok((self, rejected))
    }

    pub fn validate_selection(&self, selected: &[String]) -> Result<(), CatalogError> {
        if selected.is_empty() || selected.len() > MAX_SELECTED_SERVERS {
            return Err(CatalogError::InvalidSelection);
        }
        for (index, id) in selected.iter().enumerate() {
            if selected[..index].contains(id) || !self.servers.iter().any(|entry| entry.id == *id) {
                return Err(CatalogError::InvalidSelection);
            }
        }
        Ok(())
    }

    pub fn resolve(&self, base: &str) -> Self {
        let mut resolved = self.clone();
        for entry in &mut resolved.servers {
            let origin = if entry.url == "." { base } else { &entry.url };
            entry.url = key(origin);
        }
        resolved
    }

    /// CSP transport ports for exact configured hosts, excluding IPv6 literals.
    pub fn connect_sources(&self) -> Vec<String> {
        let mut sources = Vec::new();
        for entry in &self.servers {
            let Ok(Some(origin)) = target_origin(&entry.url) else {
                continue;
            };
            if !origin.host.contains(':') {
                for scheme in ["http", "https", "ws", "wss"] {
                    sources.push(format!("{scheme}://{}:*", origin.host));
                }
            }
            for raw in &entry.additional_origins {
                if browser_connect_source_supported(raw) {
                    sources.push(raw.clone());
                    sources.push(raw.replacen("http", "ws", 1));
                }
            }
        }
        sources
    }
}

impl ServerEntry {
    pub fn validate_discovery(&self, preflight: &Preflight) -> Result<(), DiscoveryError> {
        preflight.validate()?;
        if !preflight.base_urls().all(|origin| self.allows_origin(origin)) {
            return Err(DiscoveryError::UnapprovedOrigin);
        }
        Ok(())
    }

    /// Constrains discovery only; never grants credential access.
    pub fn allows_origin(&self, raw: &str) -> bool {
        if raw == "." {
            return true;
        }
        let Ok(Some(origin)) = target_origin(raw) else {
            return false;
        };
        let Ok(base) = target_origin(&self.url) else {
            return false;
        };
        base.is_some_and(|base| origin.host.eq_ignore_ascii_case(&base.host))
            || self.additional_origins.iter().any(|allowed| key(raw) == key(allowed))
    }
}
