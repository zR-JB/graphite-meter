//! Models the discovery and probe documents (`api/discovery.md`, `preflight.schema.json`, `probe.schema.json`).

use crate::{json, origin::BaseUrl, text};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{net::IpAddr, ops::RangeInclusive, time::Duration};

/// Clients read at most this many bytes of a control response.
pub const MAX_RESPONSE_BYTES: usize = 64 << 10;
/// Each target list holds at most this many targets, counted as sent.
pub const MAX_TARGETS: usize = 32;
/// Server names, locations, engine versions and generations hold at most this many bytes of safe text.
pub const MAX_METADATA_BYTES: usize = 256;
/// A server keeps this stage limit unless its operator sets one.
pub const DEFAULT_STAGE_LIMIT: Duration = Duration::from_mins(5);
/// A server may advertise stage limits in this range.
pub const STAGE_LIMITS: RangeInclusive<Duration> = Duration::from_secs(1)..=Duration::from_hours(24);

named! {
    pub enum ThroughputTransport {
        FetchStream => "fetch-stream",
        WebTransport => "webtransport",
        WebTransportDatagram => "webtransport-datagram",
    }
}

named! {
    /// Names a throughput target's HTTP version. On a negotiated origin the client chooses.
    pub enum Protocol {
        Http1 => "http1",
        Http2 => "http2",
        Http3 => "http3",
        Negotiated => "negotiated",
    }
}

named! {
    pub enum LatencyTransport {
        WebSocket => "websocket",
        WebTransport => "webtransport",
    }
}

named! {
    /// Says where the server read the client's address, its socket or a trusted proxy's header.
    pub enum ClientIpSource {
        Socket => "socket",
        Forwarded => "forwarded",
    }
}

named! {
    /// The probe reached the server's hop over this HTTP version.
    pub enum NegotiatedProtocol {
        Http1 => "http/1.1",
        Http2 => "h2",
        Http3 => "h3",
    }
}

/// `/preflight` tells who a server is and the targets it offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    pub server: ServerInfo,
    pub engine_version: String,
    pub generation: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Fresh owner-checked receiver checkpoints are available.
    pub upload_checkpoint: bool,
    pub max_stage_ms: u64,
    #[serde(deserialize_with = "throughput_targets")]
    pub throughput: Vec<ThroughputTarget>,
    #[serde(deserialize_with = "latency_targets")]
    pub latency: Vec<LatencyTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThroughputTarget {
    pub base_url: BaseUrl,
    pub transport: ThroughputTransport,
    pub protocol: Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyTarget {
    pub base_url: BaseUrl,
    pub transport: LatencyTransport,
}

impl Preflight {
    /// Reads a preflight. Invalid metadata, stage limit or known target refuses it, and unknown transports are skipped.
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        let preflight: Self = json::decode(data)?;
        let Self { server, engine_version, generation, capabilities } = &preflight;
        let metadata = [&server.name, &server.location, engine_version, generation];
        if generation.is_empty() || !metadata.iter().all(|text| is_metadata(text)) {
            return Err(serde_json::Error::custom("invalid discovery metadata"));
        }
        if !STAGE_LIMITS.contains(&capabilities.stage_limit()) {
            return Err(serde_json::Error::custom("invalid stage limit"));
        }
        Ok(preflight)
    }

    /// Yields every target's base URL, throughput targets first.
    pub fn base_urls(&self) -> impl Iterator<Item = &BaseUrl> {
        let throughput = self.capabilities.throughput.iter().map(|target| &target.base_url);
        throughput.chain(self.capabilities.latency.iter().map(|target| &target.base_url))
    }
}

impl Capabilities {
    /// A client may plan a stage this long against this server.
    pub fn stage_limit(&self) -> Duration {
        Duration::from_millis(self.max_stage_ms)
    }
}

pub(crate) fn is_metadata(metadata: &str) -> bool {
    metadata.len() <= MAX_METADATA_BYTES && metadata.chars().all(text::safe)
}

fn throughput_targets<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ThroughputTarget>, D::Error> {
    #[derive(Default, Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    struct Sent {
        base_url: String,
        transport: String,
        protocol: String,
    }
    let mut targets = Vec::new();
    for Sent { base_url, transport, protocol } in bounded(deserializer)? {
        if transport.is_empty() || protocol.is_empty() {
            return Err(D::Error::custom("a throughput target lacks its transport or protocol"));
        }
        let (Some(transport), Some(protocol)) = (
            ThroughputTransport::find_by(ThroughputTransport::name, &transport),
            Protocol::find_by(Protocol::name, &protocol),
        ) else {
            continue;
        };
        targets.push(ThroughputTarget { base_url: base_url_of(&base_url)?, transport, protocol });
    }
    Ok(targets)
}

fn latency_targets<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<LatencyTarget>, D::Error> {
    #[derive(Default, Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    struct Sent {
        base_url: String,
        transport: String,
    }
    let mut targets = Vec::new();
    for Sent { base_url, transport } in bounded(deserializer)? {
        if transport.is_empty() {
            return Err(D::Error::custom("a latency target lacks its transport"));
        }
        if let Some(transport) = LatencyTransport::find_by(LatencyTransport::name, &transport) {
            targets.push(LatencyTarget { base_url: base_url_of(&base_url)?, transport });
        }
    }
    Ok(targets)
}

fn bounded<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Vec<T>, D::Error> {
    let sent = Vec::<T>::deserialize(deserializer)?;
    match sent.len() {
        0..=MAX_TARGETS => Ok(sent),
        _ => Err(D::Error::custom("too many discovery targets")),
    }
}

fn base_url_of<E: serde::de::Error>(text: &str) -> Result<BaseUrl, E> {
    BaseUrl::parse(text).map_err(E::custom)
}

/// Documents spell an address family as its IP version, 4 or 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpFamily {
    V4,
    V6,
}

impl IpFamily {
    pub fn of(address: IpAddr) -> Self {
        if address.is_ipv4() { Self::V4 } else { Self::V6 }
    }

    pub fn version(self) -> u8 {
        match self {
            Self::V4 => 4,
            Self::V6 => 6,
        }
    }
}

impl Serialize for IpFamily {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.version())
    }
}

impl<'de> Deserialize<'de> for IpFamily {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u8::deserialize(deserializer)? {
            4 => Ok(Self::V4),
            6 => Ok(Self::V6),
            _ => Err(D::Error::custom("invalid client IP version")),
        }
    }
}

/// `/probe` tells how the server saw this connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub client_ip: String,
    pub client_ip_version: IpFamily,
    pub client_ip_source: ClientIpSource,
    pub protocol_negotiated: NegotiatedProtocol,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<Load>,
}

/// Records measurement-handler occupancy when the probe was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Load {
    pub active: u64,
    pub max: u64,
}

impl Probe {
    /// Reads probe evidence. Invalid evidence never reaches the caller.
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        let probe: Self = json::decode(data)?;
        if !(1..=64).contains(&probe.client_ip.len()) {
            return Err(serde_json::Error::custom("invalid probe evidence"));
        }
        if probe.load.is_some_and(|load| load.max == 0) {
            return Err(serde_json::Error::custom("invalid probe occupancy"));
        }
        Ok(probe)
    }
}
