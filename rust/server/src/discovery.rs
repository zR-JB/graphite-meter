//! Concrete discovery responses shared by HTTP listener adapters.
use crate::{
    admission::Admission,
    config::{Config, ConfigError},
    preflight::Preflight,
    probe::Probe,
};
use bytes::Bytes;
use graphite_meter_core::catalog::ServerCatalog;
use http::{Method, Request, Response, StatusCode, header, uri::Authority};
use std::{net::SocketAddr, sync::Arc};

pub struct Discovery {
    config: Arc<Config>,
    preflight: Preflight,
    probe: Probe,
}

impl Discovery {
    pub fn new(
        config: Arc<Config>,
        admission: Option<Admission>,
        bootstrap_port: Option<u16>,
    ) -> Result<Self, ConfigError> {
        // Match the Go endpoint's fallback for an unset catalogue.
        let config = if config.server_catalog.servers.is_empty() {
            let mut config = (*config).clone();
            config.server_catalog = ServerCatalog::singleton();
            Arc::new(config)
        } else {
            config
        };
        Ok(Self {
            preflight: Preflight::new(config.clone())?,
            probe: Probe::new(config.clone(), bootstrap_port, admission),
            config,
        })
    }

    pub fn respond(
        &self,
        request: &Request<()>,
        peer: SocketAddr,
    ) -> Result<Option<Response<Bytes>>, ConfigError> {
        let response = match request.uri().path() {
            "/probe" => self
                .probe
                .respond(peer, request.version(), request.headers())?,
            "/preflight" => {
                let document = self.preflight.build(authority(request)?)?;
                json_response(serde_json::to_vec(&document)?)?
            }
            "/servers" => {
                if request.method() != Method::GET {
                    return Ok(Some(
                        Response::builder()
                            .status(StatusCode::METHOD_NOT_ALLOWED)
                            .body(Bytes::new())?,
                    ));
                }
                let authority = authority(request)?;
                if authority.contains('@') {
                    return Err("request authority must not contain credentials".into());
                }
                let authority: Authority = authority.parse()?;
                let host = authority
                    .host()
                    .trim_start_matches('[')
                    .trim_end_matches(']');
                let mut catalog = self.config.server_catalog.clone();
                let local = &mut catalog.servers[0];
                local.name.clone_from(&self.config.server_name);
                local.location.clone_from(&self.config.server_location);
                local.additional_origins.extend(
                    self.preflight
                        .connect_origins(host)?
                        .into_iter()
                        .filter(|origin| {
                            origin.starts_with("http://") || origin.starts_with("https://")
                        }),
                );
                catalog.validate()?;
                let data = serde_json::to_vec(&catalog)?;
                if data.len() > 64 << 10 {
                    return Err("published catalogue exceeds 64 KiB".into());
                }
                json_response(data)?
            }
            _ => return Ok(None),
        };
        Ok(Some(response))
    }
}

fn authority(request: &Request<()>) -> Result<&str, ConfigError> {
    if let Some(authority) = request.uri().authority() {
        return Ok(authority.as_str());
    }
    Ok(request
        .headers()
        .get(header::HOST)
        .ok_or("missing request authority")?
        .to_str()?)
}

fn json_response(data: Vec<u8>) -> Result<Response<Bytes>, ConfigError> {
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Bytes::from(data))?)
}
