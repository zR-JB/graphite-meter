//! The wire rate a payload rate takes: every layer's headers at a 1,500 B MTU, as the browser estimates it
//! (`client/src/lib/compensation.ts`, its central estimate).
use crate::{
    model::{Direction, StageResult},
    net::ThroughputPath,
    run::prepare::ServerPath,
};
use graphite_meter_proto::{
    discovery::{Protocol, ThroughputTransport},
    origin::Scheme,
};

const MTU: f64 = 1500.0;
/// MAC header, frame check, preamble and the gap between frames.
const ETHERNET: f64 = 38.0;
/// TLS records, HTTP/2 and HTTP/3 DATA frames all carry up to 16 KiB.
const RECORD: f64 = 16_384.0;

/// The wire bytes each payload byte takes over `path` on IP version `ip_version`.
pub fn multiplier(path: &ThroughputPath, ip_version: u8) -> f64 {
    let quic = path.transport != ThroughputTransport::FetchStream || path.protocol == Protocol::Http3;
    let http2 = !quic && path.protocol == Protocol::Http2;
    let tls = !quic && (http2 || path.origin.scheme == Scheme::Https);
    let framing = match () {
        _ if quic => 5.0,
        _ if http2 => 9.0,
        _ => 0.0,
    };
    let application = (1.0 + framing / RECORD) * if tls { 1.0 + 22.0 / RECORD } else { 1.0 };
    let ip = if ip_version == 6 { 40.0 } else { 20.0 };
    // UDP with a QUIC short header (8 B connection ID, 2 B packet number) and AEAD tag, or TCP with timestamps.
    let transport = if quic { 8.0 + 1.0 + 8.0 + 2.0 + 16.0 } else { 20.0 + 12.0 };
    application * (MTU + ETHERNET) / (MTU - ip - transport)
}

/// A stage's wire rate in bytes per second: each server's measured rates over its own path; none while a server that
/// measured lacks its path.
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
            wire += rate * multiplier(&paths.throughput, paths.ip_version);
        }
    }
    (wire > 0.0).then_some(wire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_proto::origin::Origin;

    fn path(origin: &str, transport: ThroughputTransport, protocol: Protocol) -> ThroughputPath {
        ThroughputPath { origin: Origin::parse(origin).unwrap(), transport, protocol }
    }

    #[test]
    fn each_layer_adds_its_headers_as_the_browser_counts_them() {
        let fetch = ThroughputTransport::FetchStream;
        for (path, ip_version, expected) in [
            (path("https://meter.example", fetch, Protocol::Http1), 4, 1.001_343 * 1538.0 / 1448.0),
            (path("http://meter.example", fetch, Protocol::Http1), 4, 1538.0 / 1448.0),
            (
                path("https://meter.example", fetch, Protocol::Http2),
                6,
                1.000_549 * 1.001_343 * 1538.0 / 1428.0,
            ),
            (path("https://meter.example", fetch, Protocol::Http3), 4, 1.000_305 * 1538.0 / 1445.0),
            (
                path("https://meter.example", ThroughputTransport::WebTransport, Protocol::Http3),
                6,
                1.000_305 * 1538.0 / 1425.0,
            ),
        ] {
            let got = multiplier(&path, ip_version);
            assert!((got - expected).abs() < 1e-5, "{path:?} on IPv{ip_version}: {got} ≠ {expected}");
        }
    }
}
