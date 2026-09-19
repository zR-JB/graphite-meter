//! Connection evidence uses the accepted transport, never proxy protocol claims.
use crate::{
    admission::Admission,
    client_address,
    config::{Config, ConfigError},
};
use bytes::Bytes;
use graphite_meter_core::discovery::{Probe as Document, ProbeLoad, ProtocolNegotiated};
use http::{HeaderMap, Response, Version, header};
use std::{net::SocketAddr, sync::Arc};

pub struct Probe {
    config: Arc<Config>,
    bootstrap_port: Option<u16>,
    admission: Option<Admission>,
}

impl Probe {
    /// Set bootstrap_port only on the TCP bootstrap for the HTTP/3 listener.
    pub fn new(
        config: Arc<Config>,
        bootstrap_port: Option<u16>,
        admission: Option<Admission>,
    ) -> Self {
        Self {
            config,
            bootstrap_port,
            admission,
        }
    }

    pub fn respond(
        &self,
        peer: SocketAddr,
        version: Version,
        headers: &HeaderMap,
    ) -> Result<Response<Bytes>, ConfigError> {
        let client = client_address::resolve(peer, headers, &self.config.trusted_proxies);
        let protocol = match version {
            Version::HTTP_3 => ProtocolNegotiated::Http3,
            Version::HTTP_2 => ProtocolNegotiated::Http2,
            _ => ProtocolNegotiated::Http1,
        };
        let document = Document {
            client_ip: client.addr.to_string(),
            client_ip_version: client.version(),
            client_ip_source: client.source,
            protocol_negotiated: protocol,
            load: self.admission.as_ref().map(|admission| {
                let (active, max) = admission.load();
                ProbeLoad { active, max }
            }),
        };
        let mut response = Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CACHE_CONTROL, "no-store");
        if let Some(port) = self.bootstrap_port
            && protocol == ProtocolNegotiated::Http1
        {
            response = response
                .header(header::ALT_SVC, format!("h3=\":{port}\""))
                .header(header::CONNECTION, "close");
        }
        Ok(response.body(Bytes::from(serde_json::to_vec(&document)?))?)
    }
}
