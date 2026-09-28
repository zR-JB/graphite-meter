//! Concrete discovery responses shared by HTTP listener adapters.
use crate::{
    admission::Admission,
    config::{Config, ConfigError, NativeKind},
    preflight::{Preflight, discovery_host},
};
use bytes::Bytes;
use graphite_meter_core::origin::target_origin;
use http::{Request, Response, StatusCode, header, uri::Authority};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const MAX_HOSTS: usize = 64;

pub struct Discovery {
    config: Arc<Config>,
    preflight: Preflight,
    admission: Admission,
    configured: HashMap<String, Arc<HostResponses>>,
    hosts: Mutex<HashMap<String, Arc<HostResponses>>>,
}

struct HostResponses {
    preflight: Bytes,
    catalog: Option<Bytes>,
}

impl Discovery {
    pub fn new(config: Arc<Config>, admission: Admission) -> Result<Self, ConfigError> {
        let preflight = Preflight::new(config.clone())?;
        let public = &config.public;
        let origins = ["http://localhost", config.auth.public_url.as_str()]
            .into_iter()
            .chain(NativeKind::ALL.map(|kind| config.listener(kind).public_origin.as_str()))
            .chain(
                public
                    .both
                    .iter()
                    .chain(&public.throughput)
                    .chain(&public.latency)
                    .map(String::as_str),
            );
        let mut configured = HashMap::new();
        for origin in origins {
            if let Ok(Some(origin)) = target_origin(origin)
                && discovery_host(&origin.host) == origin.host
                && !configured.contains_key(&origin.host)
            {
                let responses = build(&config, &preflight, &origin.host)?;
                configured.insert(origin.host, Arc::new(responses));
            }
        }
        Ok(Self {
            preflight,
            admission,
            config,
            configured,
            hosts: Mutex::default(),
        })
    }

    pub fn respond(&self, request: &Request<()>, peer: SocketAddr) -> Result<Option<Response<Bytes>>, ConfigError> {
        let response = match request.uri().path() {
            "/probe" => crate::probe::respond(
                &self.admission,
                &self.config.trusted_proxies,
                peer,
                request.version(),
                request.headers(),
            )?,
            "/preflight" => json_response(self.for_host(&request_host(request))?.preflight.clone())?,
            "/servers" => match &self.for_host(&request_host(request))?.catalog {
                Some(catalog) => json_response(catalog.clone())?,
                None => Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                    .body(Bytes::from_static(b"server catalogue unavailable\n"))?,
            },
            _ => return Ok(None),
        };
        Ok(Some(response))
    }

    fn for_host(&self, host: &str) -> Result<Arc<HostResponses>, ConfigError> {
        let host = discovery_host(host);
        if let Some(responses) = self.configured.get(host) {
            return Ok(responses.clone());
        }
        if let Some(responses) = self.hosts.lock().expect("discovery cache poisoned").get(host) {
            return Ok(responses.clone());
        }
        let responses = Arc::new(build(&self.config, &self.preflight, host)?);
        let mut hosts = self.hosts.lock().expect("discovery cache poisoned");
        if hosts.len() >= MAX_HOSTS
            && let Some(evicted) = hosts.keys().next().cloned()
        {
            hosts.remove(&evicted);
        }
        hosts.insert(host.to_owned(), responses.clone());
        Ok(responses)
    }
}

/// An invalid catalogue is logged once per build and answered 500 until the host is built again.
fn build(config: &Config, preflight: &Preflight, host: &str) -> Result<HostResponses, ConfigError> {
    let mut catalog = config.published_catalog();
    catalog.servers[0].additional_origins.extend(
        preflight
            .connect_origins(host)?
            .into_iter()
            .filter(|origin| origin.starts_with("http://") || origin.starts_with("https://")),
    );
    let data = serde_json::to_vec(&catalog)?;
    let refused = match catalog.validate() {
        Err(error) => Some(error.to_string()),
        Ok(()) if data.len() > 64 << 10 => Some("published catalogue exceeds 64 KiB".into()),
        Ok(()) => None,
    };
    if let Some(error) = &refused {
        crate::log!("[gm:discovery] server catalogue for host {host:?}: {error:?}");
    }
    Ok(HostResponses {
        preflight: serde_json::to_vec(&preflight.build(host)?)?.into(),
        catalog: refused.is_none().then(|| data.into()),
    })
}

pub(crate) fn request_host<B>(request: &Request<B>) -> String {
    request
        .uri()
        .authority()
        .cloned()
        .or_else(|| request.headers().get(header::HOST)?.to_str().ok()?.parse().ok())
        .filter(|authority: &Authority| !authority.as_str().contains('@'))
        .map_or_else(
            || "localhost".into(),
            |authority| authority.host().trim_start_matches('[').trim_end_matches(']').into(),
        )
}

fn json_response(data: Bytes) -> Result<Response<Bytes>, ConfigError> {
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(data)?)
}
