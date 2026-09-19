//! Validated public discovery and connection evidence; no derived measurements.

use crate::{origin::target_origin, wire::decode_json};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    InvalidJson,
    InvalidMetadata,
    InvalidTargets,
    InvalidOrigin,
    InvalidProbe,
    InvalidLoad,
    UnapprovedOrigin,
}
impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidJson => "invalid discovery JSON",
            Self::InvalidMetadata => "invalid discovery metadata",
            Self::InvalidTargets => "invalid discovery target lists",
            Self::InvalidOrigin => "invalid discovery target origin",
            Self::InvalidProbe => "invalid probe evidence",
            Self::InvalidLoad => "invalid probe occupancy",
            Self::UnapprovedOrigin => "server advertised an unapproved origin",
        })
    }
}
impl std::error::Error for DiscoveryError {}

fn null_default<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    deserializer: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    #[serde(default, deserialize_with = "null_default")]
    pub server: ServerInfo,
    #[serde(default, deserialize_with = "null_default")]
    pub engine_version: String,
    pub generation: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub upload_checkpoint: bool,
    pub throughput: Vec<ThroughputTarget>,
    pub latency: Vec<LatencyTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThroughputTransport {
    FetchStream,
    #[serde(rename = "webtransport")]
    WebTransport,
    #[serde(rename = "webtransport-datagram")]
    WebTransportDatagram,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http1,
    Http2,
    Http3,
    Negotiated,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LatencyTransport {
    WebSocket,
    WebTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThroughputTarget {
    pub base_url: String,
    pub transport: ThroughputTransport,
    pub protocol: Protocol,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyTarget {
    pub base_url: String,
    pub transport: LatencyTransport,
}

impl Preflight {
    pub fn decode(data: &[u8]) -> Result<Self, DiscoveryError> {
        let value: Self = decode_json(data).map_err(|_| DiscoveryError::InvalidJson)?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        if self.server.name.len() > 256
            || self.server.location.len() > 256
            || self.engine_version.len() > 256
            || self.generation.is_empty()
            || self.generation.len() > 256
        {
            return Err(DiscoveryError::InvalidMetadata);
        }
        if self.capabilities.throughput.len() > 32 || self.capabilities.latency.len() > 32 {
            return Err(DiscoveryError::InvalidTargets);
        }
        let throughput_origins = self
            .capabilities
            .throughput
            .iter()
            .map(|target| &target.base_url);
        let latency_origins = self
            .capabilities
            .latency
            .iter()
            .map(|target| &target.base_url);
        for origin in throughput_origins.chain(latency_origins) {
            target_origin(origin).map_err(|_| DiscoveryError::InvalidOrigin)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientIpSource {
    Socket,
    Forwarded,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolNegotiated {
    #[serde(rename = "http/1.1")]
    Http1,
    #[serde(rename = "h2")]
    Http2,
    #[serde(rename = "h3")]
    Http3,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub client_ip: String,
    pub client_ip_version: u8,
    pub client_ip_source: ClientIpSource,
    pub protocol_negotiated: ProtocolNegotiated,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<ProbeLoad>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeLoad {
    #[serde(default, deserialize_with = "null_default")]
    pub active: usize,
    pub max: usize,
}
impl Probe {
    pub fn decode(data: &[u8]) -> Result<Self, DiscoveryError> {
        let value: Self = decode_json(data).map_err(|_| DiscoveryError::InvalidJson)?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        if self.client_ip.is_empty()
            || self.client_ip.len() > 64
            || !matches!(self.client_ip_version, 4 | 6)
        {
            return Err(DiscoveryError::InvalidProbe);
        }
        // Match Go's signed int range even though occupancy is nonnegative.
        if self.load.is_some_and(|load| {
            load.max == 0 || load.active > isize::MAX as usize || load.max > isize::MAX as usize
        }) {
            return Err(DiscoveryError::InvalidLoad);
        }
        Ok(())
    }
}
