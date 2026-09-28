//! Connection evidence uses the accepted transport, never proxy protocol claims.
use crate::{admission::Admission, client_address, config::ConfigError};
use bytes::Bytes;
use graphite_meter_core::discovery::{Probe, ProbeLoad, ProtocolNegotiated};
use http::{HeaderMap, Response, Version, header};
use ipnet::IpNet;
use std::net::SocketAddr;

pub(crate) fn respond(
    admission: &Admission,
    trusted: &[IpNet],
    peer: SocketAddr,
    version: Version,
    headers: &HeaderMap,
) -> Result<Response<Bytes>, ConfigError> {
    let client = client_address::resolve(peer, headers, trusted);
    if !client.usable {
        return Ok(Response::builder()
            .status(400)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
            .body(Bytes::from_static(b"ambiguous client address\n"))?);
    }
    let (active, max) = admission.load();
    let document = Probe {
        client_ip: client.addr.to_string(),
        client_ip_version: client.version(),
        client_ip_source: client.source,
        protocol_negotiated: match version {
            Version::HTTP_3 => ProtocolNegotiated::Http3,
            Version::HTTP_2 => ProtocolNegotiated::Http2,
            _ => ProtocolNegotiated::Http1,
        },
        load: Some(ProbeLoad { active, max }),
    };
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Bytes::from(serde_json::to_vec(&document)?))?)
}
