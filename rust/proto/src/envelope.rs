//! The envelope every preflight and probe carries (`api/discovery.md#the-envelope`): the part of a document no
//! protocol changes, read before the rest, so that a client names a server it cannot speak to.

use crate::discovery::is_metadata;
use serde_json::Value;

/// The API protocol this build speaks. A server raises it with each change that older clients cannot read.
pub const PROTOCOL: u64 = 2;
/// The first release of each protocol (`api/protocols.txt`).
pub const PROTOCOLS: [(u64, &str); 2] = [(1, "0.1.0"), (2, "0.13.5")];
/// What a preflight with a lease but no `protocol` speaks: 0.13.5's.
pub const LEASED: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// `None` when missing or no safe text.
    pub engine_version: Option<String>,
    pub protocol: u64,
}

impl Envelope {
    /// Reads a preflight's envelope, whatever else it holds; `None` without one. A preflight without `protocol` is
    /// from before the envelope: protocol 1, or `LEASED` with a lease.
    pub fn preflight(data: &[u8]) -> Option<Self> {
        Self::read(data, true)
    }

    /// Reads a probe's envelope, which older servers do not send.
    pub fn probe(data: &[u8]) -> Option<Self> {
        Self::read(data, false)
    }

    fn read(data: &[u8], preflight: bool) -> Option<Self> {
        let document = crate::json::decode::<Value>(data).ok()?;
        let document = document.as_object()?;
        let version = document.get("engineVersion").and_then(Value::as_str);
        let protocol = match document.get("protocol") {
            Some(protocol) => protocol.as_u64().filter(|&protocol| protocol >= 1)?,
            None if !preflight || version.is_none() => return None,
            None if document.contains_key("lease") => LEASED,
            None => 1,
        };
        let engine_version = version
            .filter(|version| !version.is_empty() && is_metadata(version))
            .map(str::to_owned);
        Some(Self {
            engine_version,
            protocol,
        })
    }

    /// Why this build, of version `own`, cannot use `name` at this envelope; `None` when it can.
    pub fn mismatch(&self, name: &str, own: &str) -> Option<String> {
        let Self {
            engine_version,
            protocol,
        } = self;
        let first = PROTOCOLS[PROTOCOLS.len() - 1].1;
        let release = engine_version.as_ref().map_or_else(
            || "an unknown Graphite Meter release".to_owned(),
            |version| format!("Graphite Meter {version}"),
        );
        match protocol.cmp(&PROTOCOL) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some(format!(
                "{name} runs {release} (protocol {protocol}); this client needs protocol \
                 {PROTOCOL}, from {first}. Update {name}."
            )),
            std::cmp::Ordering::Greater => Some(format!(
                "{name} runs {release}, newer than this client ({own}); update this client."
            )),
        }
    }
}
