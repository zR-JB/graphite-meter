//! Connection evidence uses the accepted transport, never proxy protocol claims.
use crate::{
    admission::Admission,
    client_address,
    http::response::{json_response, text_body},
};
use bytes::Bytes;
use graphite_meter_core::discovery::{Probe, ProbeLoad, ProtocolNegotiated};
use http::{HeaderMap, Response, StatusCode, Version};
use ipnet::IpNet;
use std::net::SocketAddr;

/// Marks the probe's own answers, which the HTTP/3 companion points at its QUIC port, as Go's bootstrap probe does.
#[derive(Clone, Copy)]
pub(crate) struct Answer;

pub(crate) fn respond(
    admission: &Admission,
    trusted: &[IpNet],
    peer: SocketAddr,
    version: Version,
    headers: &HeaderMap,
) -> Response<Bytes> {
    let client = client_address::resolve(peer, headers, trusted);
    let mut response = if client.usable {
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
        json_response(serde_json::to_vec(&document).expect("a probe document serializes"))
    } else {
        text_body(StatusCode::BAD_REQUEST, "ambiguous client address")
    };
    response.extensions_mut().insert(Answer);
    response
}
