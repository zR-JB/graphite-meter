//! The originating server's catalogue (`api/discovery.md`, `servers.schema.json`).

use crate::{
    discovery::{Preflight, is_metadata},
    json,
    origin::{BaseUrl, Origin},
};
use serde::{Deserialize, Serialize, de::Error as _};
use std::{collections::HashSet, fmt};

/// A catalogue holds at most this many servers, `self` included.
pub const MAX_SERVERS: usize = 32;
/// A selection holds at most this many servers.
pub const MAX_SELECTED: usize = 4;
/// An entry names at most this many additional origins.
pub const MAX_ADDITIONAL_ORIGINS: usize = 32;

/// A catalogue server ID: 1–64 ASCII letters, digits, `.`, `_` or `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ServerId(String);

impl ServerId {
    pub fn parse(text: &str) -> Option<Self> {
        let id_byte = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        ((1..=64).contains(&text.len()) && text.bytes().all(id_byte)).then(|| Self(text.into()))
    }

    /// `self`: the server that serves the catalogue.
    pub fn own() -> Self {
        Self("self".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One measurement server and the origins its discovery may advertise beyond its own host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerEntry {
    pub id: ServerId,
    pub url: BaseUrl,
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub location: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional_origins: Vec<Origin>,
}

/// `/servers`: the catalogue, `self` first, and the servers selected by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCatalog {
    pub default_selection: Vec<ServerId>,
    pub servers: Vec<ServerEntry>,
}

/// Why a catalogue, or one entry of it, is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogError {
    Servers,
    Identity,
    Origin,
    Duplicate,
    AdditionalOrigins,
    Selection,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Servers => "a catalogue lists self first and at most 31 servers after it",
            Self::Identity => "invalid catalogue server identity",
            Self::Origin => "invalid catalogue origin",
            Self::Duplicate => "duplicate catalogue server",
            Self::AdditionalOrigins => "too many additional origins",
            Self::Selection => "select one to four distinct catalogue servers",
        })
    }
}

impl std::error::Error for CatalogError {}

/// A received entry left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub id: String,
    pub error: CatalogError,
}

/// A received catalogue: its valid entries, and the entries left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub catalog: ServerCatalog,
    pub rejected: Vec<Rejected>,
}

impl ServerCatalog {
    /// The catalogue of a server configured without one.
    pub fn singleton() -> Self {
        let own = ServerEntry {
            id: ServerId::own(),
            url: BaseUrl::Served,
            name: "graphite-meter".into(),
            location: String::new(),
            additional_origins: Vec::new(),
        };
        Self { default_selection: vec![ServerId::own()], servers: vec![own] }
    }

    /// Checks a catalogue before a server publishes it.
    pub fn validate(&self) -> Result<(), CatalogError> {
        if !self.servers.first().is_some_and(|first| first.id == ServerId::own()) || self.servers.len() > MAX_SERVERS {
            return Err(CatalogError::Servers);
        }
        for (index, entry) in self.servers.iter().enumerate() {
            entry.check(&self.servers[..index])?;
        }
        self.validate_selection(&self.default_selection)
    }

    /// Checks a selection: one to four distinct servers of this catalogue.
    pub fn validate_selection(&self, selected: &[ServerId]) -> Result<(), CatalogError> {
        let mut seen = HashSet::new();
        let known = |id: &ServerId| self.servers.iter().any(|entry| entry.id == *id);
        let valid =
            (1..=MAX_SELECTED).contains(&selected.len()) && selected.iter().all(|id| seen.insert(id) && known(id));
        valid.then_some(()).ok_or(CatalogError::Selection)
    }

    /// Reads a received catalogue, dropping invalid entries but `self`; an emptied selection falls back to `self`.
    pub fn decode(data: &[u8]) -> Result<Received, serde_json::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Sent {
            #[serde(default)]
            default_selection: Vec<String>,
            servers: Vec<SentEntry>,
        }
        let sent: Sent = json::decode(data)?;
        let refused = serde_json::Error::custom;
        if sent.servers.len() > MAX_SERVERS {
            return Err(refused(CatalogError::Servers));
        }
        let (mut servers, mut rejected) = (Vec::new(), Vec::new());
        for entry in sent.servers {
            let id = entry.id.clone();
            match entry.received(&servers) {
                Ok(entry) => servers.push(entry),
                Err(error) if id == "self" => return Err(refused(error)),
                Err(error) => rejected.push(Rejected { id, error }),
            }
        }
        let known = |id: &ServerId| servers.iter().any(|entry| entry.id == *id);
        let parsed = sent.default_selection.iter().filter_map(|id| ServerId::parse(id));
        let mut default_selection: Vec<_> = parsed.filter(known).collect();
        if default_selection.is_empty() {
            default_selection.push(ServerId::own());
        }
        let catalog = Self { default_selection, servers };
        catalog.validate().map_err(refused)?;
        Ok(Received { catalog, rejected })
    }
}

impl ServerEntry {
    /// Whether this entry's discovery from `served` may list all `preflight` targets: served-host ports, exact origins.
    pub fn approves(&self, served: &Origin, preflight: &Preflight) -> bool {
        preflight.base_urls().all(|base| match base {
            BaseUrl::Served => true,
            BaseUrl::Origin(target) => target.host == served.host || self.additional_origins.contains(target),
        })
    }

    /// This entry's faults, given the entries before it.
    fn check(&self, earlier: &[Self]) -> Result<(), CatalogError> {
        if !is_metadata(&self.name) || !is_metadata(&self.location) {
            return Err(CatalogError::Identity);
        }
        if self.url == BaseUrl::Served && self.id != ServerId::own() {
            return Err(CatalogError::Origin);
        }
        if earlier.iter().any(|prior| prior.id == self.id || prior.url == self.url) {
            return Err(CatalogError::Duplicate);
        }
        if self.additional_origins.len() > MAX_ADDITIONAL_ORIGINS {
            return Err(CatalogError::AdditionalOrigins);
        }
        Ok(())
    }
}

/// A catalogue entry as sent, read field by field so one invalid entry is refused alone.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SentEntry {
    id: String,
    url: String,
    name: String,
    location: String,
    additional_origins: Vec<String>,
}

impl SentEntry {
    fn received(self, earlier: &[ServerEntry]) -> Result<ServerEntry, CatalogError> {
        let Self { id, url, name, location, additional_origins } = self;
        let id = ServerId::parse(&id).ok_or(CatalogError::Identity)?;
        let url = match url.as_str() {
            "." => BaseUrl::Served,
            url => BaseUrl::Origin(received_origin(url)?),
        };
        let additional_origins = additional_origins
            .iter()
            .map(|origin| received_origin(origin))
            .collect::<Result<_, _>>()?;
        let entry = ServerEntry { id, url, name, location, additional_origins };
        entry.check(earlier)?;
        Ok(entry)
    }
}

/// A catalogue origin, which may end in one slash.
fn received_origin(text: &str) -> Result<Origin, CatalogError> {
    Origin::parse_received(text.strip_suffix('/').unwrap_or(text)).map_err(|_| CatalogError::Origin)
}
