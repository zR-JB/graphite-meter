//! Public measurement authorities and deterministic discovery boundaries.

use crate::{
    discovery::{DiscoveryError, Preflight},
    origin::{browser_connect_source_supported, canonical_origin, key, target_origin},
};
use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_CATALOG_SERVERS: usize = 32;
pub const MAX_SELECTED_SERVERS: usize = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerEntry {
    pub id: String,
    pub url: String,
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub location: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional_origins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCatalog {
    #[serde(default)]
    pub default_selection: Vec<String>,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogError {
    InvalidServers,
    InvalidIdentity,
    DuplicateServer,
    InvalidOrigin,
    TooManyAdditionalOrigins,
    InvalidSelection,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidServers => {
                "catalogue requires self followed by at most 31 additional servers"
            }
            Self::InvalidIdentity => "invalid catalogue server identity",
            Self::DuplicateServer => "duplicate catalogue server",
            Self::InvalidOrigin => "invalid catalogue origin",
            Self::TooManyAdditionalOrigins => "too many additional origins",
            Self::InvalidSelection => "select one to four distinct known servers",
        })
    }
}
impl std::error::Error for CatalogError {}

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
        if self.servers.is_empty()
            || self.servers.len() > MAX_CATALOG_SERVERS
            || self.servers[0].id != "self"
        {
            return Err(CatalogError::InvalidServers);
        }
        for (index, entry) in self.servers.iter().enumerate() {
            let valid_id = !entry.id.is_empty()
                && entry.id.len() <= 64
                && entry
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
            let has_control_characters = entry
                .name
                .bytes()
                .chain(entry.location.bytes())
                .any(|byte| byte < 32 || byte == 127);
            if !valid_id
                || entry.name.len() > 256
                || entry.location.len() > 256
                || has_control_characters
            {
                return Err(CatalogError::InvalidIdentity);
            }
            let origin_key = key(&entry.url);
            if self.servers[..index]
                .iter()
                .any(|prior| prior.id == entry.id || key(&prior.url) == origin_key)
            {
                return Err(CatalogError::DuplicateServer);
            }
            if entry.url == "." {
                if entry.id != "self" {
                    return Err(CatalogError::InvalidOrigin);
                }
            } else if canonical_origin(&entry.url).is_err() {
                return Err(CatalogError::InvalidOrigin);
            }
            if entry.additional_origins.len() > 32 {
                return Err(CatalogError::TooManyAdditionalOrigins);
            }
            if entry
                .additional_origins
                .iter()
                .any(|raw| canonical_origin(raw).is_err())
            {
                return Err(CatalogError::InvalidOrigin);
            }
        }
        self.validate_selection(&self.default_selection)
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
        let throughput_origins = preflight
            .capabilities
            .throughput
            .iter()
            .map(|target| &target.base_url);
        let latency_origins = preflight
            .capabilities
            .latency
            .iter()
            .map(|target| &target.base_url);
        for origin in throughput_origins.chain(latency_origins) {
            if !self.allows_origin(origin) {
                return Err(DiscoveryError::UnapprovedOrigin);
            }
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
        let base = match target_origin(&self.url) {
            Ok(base) => base,
            Err(_) => return false,
        };
        if base.is_some_and(|base| origin.host.eq_ignore_ascii_case(&base.host)) {
            return true;
        }
        self.additional_origins
            .iter()
            .any(|allowed| key(raw) == key(allowed))
    }
}
