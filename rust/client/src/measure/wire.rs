//! Converts a payload rate to the wire rate with every layer's headers at a 1,500 B MTU, as the browser estimates it
//! (`client/src/engine/measure/overhead.ts`, its central estimate).
use crate::{
    model::{Direction, StageResult},
    net::ThroughputPath,
    run::prepare::ServerPath,
};
use graphite_meter_proto::{
    discovery::{IpFamily, Protocol, ThroughputTransport},
    origin::Scheme,
};

const MTU: f64 = 1500.0;
/// Ethernet adds the MAC header, frame check, preamble and the gap between frames.
const ETHERNET: f64 = 38.0;
/// TLS records, HTTP/2 and HTTP/3 DATA frames all carry up to 16 KiB.
const RECORD: f64 = 16_384.0;

/// Returns the wire bytes each payload byte takes over `path` in address family `family`.
pub fn multiplier(path: &ThroughputPath, family: IpFamily) -> f64 {
    let quic = path.transport != ThroughputTransport::FetchStream || path.protocol == Protocol::Http3;
    let http2 = !quic && path.protocol == Protocol::Http2;
    let tls = !quic && (http2 || path.origin.scheme == Scheme::Https);
    let framing = match () {
        _ if quic => 5.0,
        _ if http2 => 9.0,
        _ => 0.0,
    };
    let application = (1.0 + framing / RECORD) * if tls { 1.0 + 22.0 / RECORD } else { 1.0 };
    let ip = match family {
        IpFamily::V4 => 20.0,
        IpFamily::V6 => 40.0,
    };
    // QUIC rides UDP with a short header (8 B connection ID, 2 B packet number) and an AEAD tag. TCP has timestamps.
    let transport = if quic { 8.0 + 1.0 + 8.0 + 2.0 + 16.0 } else { 20.0 + 12.0 };
    application * (MTU + ETHERNET) / (MTU - ip - transport)
}

/// Sums each server's measured rates over its own path into the stage's wire rate in bytes per second. Returns none
/// while a server that measured lacks its path.
pub fn wire_rate(result: &StageResult, servers: &[ServerPath]) -> Option<f64> {
    let mut wire = 0.0;
    for server in &result.servers {
        let rate: f64 = Direction::BOTH
            .iter()
            .filter_map(|&direction| Some(server.throughput[direction]?.rate?.mean))
            .sum();
        if rate > 0.0 {
            let prepared = servers.iter().find(|prepared| prepared.id == server.server)?;
            let paths = prepared.path.as_ref().ok()?;
            wire += rate * multiplier(&paths.throughput, paths.ip_family);
        }
    }
    (wire > 0.0).then_some(wire)
}
