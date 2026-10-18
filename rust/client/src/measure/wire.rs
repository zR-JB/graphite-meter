//! The wire rate a payload rate takes at a 1,500 B MTU, as `client/src/engine/measure/overhead.ts` estimates it.
use crate::{model::StageResult, net::ThroughputPath, run::prepare::ServerPath};
use graphite_meter_proto::{
    discovery::{IpFamily, Protocol, ThroughputTransport},
    origin::Scheme,
};

const MTU: f64 = 1500.0;
/// MAC header, frame check, preamble and the gap between frames.
const ETHERNET: f64 = 38.0;
/// TLS records, HTTP/2 and HTTP/3 DATA frames all carry up to 16 KiB.
const RECORD: f64 = 16_384.0;

pub fn multiplier(path: &ThroughputPath, family: IpFamily) -> f64 {
    let quic =
        path.transport != ThroughputTransport::FetchStream || path.protocol == Protocol::Http3;
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
    // UDP with a QUIC short header (8 B connection ID, 2 B packet number) and AEAD tag, or TCP with timestamps.
    let transport = if quic {
        8.0 + 1.0 + 8.0 + 2.0 + 16.0
    } else {
        20.0 + 12.0
    };
    application * (MTU + ETHERNET) / (MTU - ip - transport)
}

/// A stage's wire rate in bytes per second; none while a server that measured lacks its path.
pub fn wire_rate(result: &StageResult, servers: &[ServerPath]) -> Option<f64> {
    let mut wire = 0.0;
    for server in &result.servers {
        let rate = server.throughput.mean();
        if rate > 0.0 {
            let prepared = servers
                .iter()
                .find(|prepared| prepared.id == server.server)?;
            let paths = prepared.path.as_ref().ok()?;
            wire += rate * multiplier(&paths.throughput, paths.ip_family);
        }
    }
    (wire > 0.0).then_some(wire)
}
