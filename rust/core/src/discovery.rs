//! Validated public discovery and connection evidence; no derived measurements.

use crate::{origin::target_origin, text::label, wire::decode_json};
use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

pub const MIN_STAGE_LIMIT: Duration = Duration::from_secs(1);
pub const DEFAULT_STAGE_LIMIT: Duration = Duration::from_secs(300);
pub const MAX_STAGE_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);

errors! {
    pub enum DiscoveryError {
        InvalidJson => "invalid discovery JSON",
        InvalidMetadata => "invalid discovery metadata",
        InvalidTargets => "invalid discovery target lists",
        InvalidStageLimit => "invalid stage limit",
        InvalidOrigin => "invalid discovery target origin",
        InvalidProbe => "invalid probe evidence",
        InvalidLoad => "invalid probe occupancy",
        UnapprovedOrigin => "server advertised an unapproved origin",
    }
}

pub(crate) fn null_default<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(
    deserializer: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_default", skip_serializing_if = "String::is_empty")]
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
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub upload_checkpoint: bool,
    #[serde(default, deserialize_with = "null_default", skip_serializing_if = "is_zero")]
    pub max_stage_ms: i64,
    #[serde(deserialize_with = "throughput_targets")]
    pub throughput: Vec<ThroughputTarget>,
    #[serde(deserialize_with = "latency_targets")]
    pub latency: Vec<LatencyTarget>,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

impl Capabilities {
    pub fn stage_limit(&self) -> Duration {
        if self.max_stage_ms == 0 {
            DEFAULT_STAGE_LIMIT
        } else {
            Duration::from_millis(self.max_stage_ms as u64)
        }
    }
}

fn throughput_targets<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ThroughputTarget>, D::Error> {
    known_targets::<_, _, true>(
        deserializer,
        &[
            ("transport", &["fetch-stream", "webtransport", "webtransport-datagram"]),
            ("protocol", &["http1", "http2", "http3", "negotiated"]),
        ],
    )
}

fn latency_targets<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<LatencyTarget>, D::Error> {
    known_targets::<_, _, false>(deserializer, &[("transport", &["websocket", "webtransport"])])
}

/// A target's members that pick and build it, its protocol only where it has one; serde skips the
/// others unparsed, as Go does.
struct Target<const PROTOCOL: bool>(serde_json::Map<String, serde_json::Value>);

impl<'de, const PROTOCOL: bool> Deserialize<'de> for Target<PROTOCOL> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let members = &["baseUrl", "transport", "protocol"][..2 + usize::from(PROTOCOL)];
        deserializer.deserialize_map(crate::wire::Members(members)).map(Self)
    }
}

fn known_targets<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned, const PROTOCOL: bool>(
    deserializer: D,
    fields: &[(&str, &[&str])],
) -> Result<Vec<T>, D::Error> {
    let targets = Vec::<Target<PROTOCOL>>::deserialize(deserializer)?;
    if targets.len() > 32 {
        return Err(serde::de::Error::custom("too many targets"));
    }
    let mut known = Vec::new();
    for Target(target) in targets {
        let mut supported = true;
        for (field, values) in fields {
            let value = target
                .get(*field)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| serde::de::Error::custom(format!("missing target {field}")))?;
            supported &= values.contains(&value);
        }
        if supported {
            let target = serde_json::Value::Object(target);
            known.push(serde_json::from_value(target).map_err(serde::de::Error::custom)?);
        }
    }
    Ok(known)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// A preflight as a client receives it: international target hosts become punycode first,
    /// as Go's client dials them.
    pub fn decode_received(data: &[u8]) -> Result<Self, DiscoveryError> {
        let mut value: Self = decode_json(data).map_err(|_| DiscoveryError::InvalidJson)?;
        for base_url in value.base_urls_mut() {
            if let Ok(origin) = crate::origin::ascii_origin(base_url) {
                *base_url = origin;
            }
        }
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        let metadata = [&self.server.name, &self.server.location, &self.engine_version, &self.generation];
        if self.generation.is_empty() || !metadata.into_iter().all(|text| label(text)) {
            return Err(DiscoveryError::InvalidMetadata);
        }
        if self.capabilities.throughput.len() > 32 || self.capabilities.latency.len() > 32 {
            return Err(DiscoveryError::InvalidTargets);
        }
        let limit = self.capabilities.max_stage_ms;
        if limit != 0 && !(MIN_STAGE_LIMIT.as_millis() as i64..=MAX_STAGE_LIMIT.as_millis() as i64).contains(&limit) {
            return Err(DiscoveryError::InvalidStageLimit);
        }
        for origin in self.base_urls() {
            target_origin(origin).map_err(|_| DiscoveryError::InvalidOrigin)?;
        }
        Ok(())
    }
    pub fn base_urls(&self) -> impl Iterator<Item = &String> {
        let throughput = self.capabilities.throughput.iter().map(|target| &target.base_url);
        throughput.chain(self.capabilities.latency.iter().map(|target| &target.base_url))
    }
    pub fn resolve_self(&mut self, origin: &str) {
        for base_url in self.base_urls_mut().filter(|base_url| *base_url == ".") {
            *base_url = origin.to_owned();
        }
    }
    fn base_urls_mut(&mut self) -> impl Iterator<Item = &mut String> {
        let targets = &mut self.capabilities;
        let throughput = targets.throughput.iter_mut().map(|target| &mut target.base_url);
        throughput.chain(targets.latency.iter_mut().map(|target| &mut target.base_url))
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
        if self.client_ip.is_empty() || self.client_ip.len() > 64 || !matches!(self.client_ip_version, 4 | 6) {
            return Err(DiscoveryError::InvalidProbe);
        }
        // Match Go's signed int range even though occupancy is nonnegative.
        let invalid =
            |load: ProbeLoad| load.max == 0 || load.active > isize::MAX as usize || load.max > isize::MAX as usize;
        if self.load.is_some_and(invalid) {
            return Err(DiscoveryError::InvalidLoad);
        }
        Ok(())
    }
}
