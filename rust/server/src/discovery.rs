//! Discovery responses and the page's connect sources, built once per request hostname as Go's hostDiscovery.
use crate::{
    admission::Admission,
    config::{AuthMode, Config, ConfigError, NativeKind, ValidatedConfig},
    http::response::{json_response, text_body},
    preflight::{Preflight, connect_origins, discovery_host},
    sync::lock,
};
use bytes::Bytes;
use graphite_meter_core::{
    origin::{browser_connect_source_supported, canonical_origin, target_origin},
    route::Route,
};
use http::{Request, Response, StatusCode, header, uri::Authority};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

const MAX_HOSTS: usize = 64;

pub struct Discovery {
    config: Arc<ValidatedConfig>,
    preflight: Preflight,
    admission: Admission,
    /// Under authentication every page names the public host, as Go's authenticated page policy does.
    page_host: Option<String>,
    configured: HashMap<String, Arc<HostResponses>>,
    hosts: Mutex<HashMap<String, Arc<HostResponses>>>,
}

struct HostResponses {
    preflight: Bytes,
    catalog: Option<Bytes>,
    page_sources: Arc<[String]>,
}

impl Discovery {
    pub fn new(config: Arc<ValidatedConfig>, admission: Admission) -> Result<Self, ConfigError> {
        let preflight = Preflight::new(config.clone())?;
        let page_host = if config.auth.mode == AuthMode::Off {
            None
        } else {
            Some(
                target_origin(&config.auth.public_url)?
                    .ok_or("missing authentication origin")?
                    .host,
            )
        };
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
            page_host,
            config,
            configured,
            hosts: Mutex::default(),
        })
    }

    /// The discovery routes' answers; `None` for any other route.
    pub fn respond(
        &self,
        route: Route,
        request: &Request<()>,
        peer: SocketAddr,
    ) -> Result<Option<Response<Bytes>>, ConfigError> {
        let response = match route {
            Route::Probe => crate::probe::respond(
                &self.admission,
                &self.config.trusted_proxies,
                peer,
                request.version(),
                request.headers(),
            )?,
            Route::Preflight => json_response(self.for_host(&request_host(request))?.preflight.clone()),
            Route::Servers => match &self.for_host(&request_host(request))?.catalog {
                Some(catalog) => json_response(catalog.clone()),
                None => text_body(StatusCode::INTERNAL_SERVER_ERROR, "server catalogue unavailable"),
            },
            _ => return Ok(None),
        };
        Ok(Some(response))
    }

    pub(crate) fn page_sources(&self, host: &str) -> Result<Arc<[String]>, ConfigError> {
        Ok(self
            .for_host(self.page_host.as_deref().unwrap_or(host))?
            .page_sources
            .clone())
    }

    fn for_host(&self, host: &str) -> Result<Arc<HostResponses>, ConfigError> {
        let host = discovery_host(host);
        if let Some(responses) = self.configured.get(host) {
            return Ok(responses.clone());
        }
        if let Some(responses) = lock(&self.hosts).get(host) {
            return Ok(responses.clone());
        }
        let responses = Arc::new(build(&self.config, &self.preflight, host)?);
        let mut hosts = lock(&self.hosts);
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
    let document = preflight.build(host)?;
    let connect = connect_origins(&document);
    let mut catalog = config.published_catalog();
    catalog.servers[0].additional_origins.extend(
        connect
            .iter()
            .filter(|origin| origin.starts_with("http://") || origin.starts_with("https://"))
            .cloned(),
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
    let mut page_sources = config.server_catalog.connect_sources();
    page_sources.extend(connect.into_iter().filter(|source| {
        let http = source.replacen("wss://", "https://", 1).replacen("ws://", "http://", 1);
        canonical_origin(&http).is_ok() && browser_connect_source_supported(source)
    }));
    page_sources.sort_unstable();
    page_sources.dedup();
    Ok(HostResponses {
        preflight: serde_json::to_vec(&document)?.into(),
        catalog: refused.is_none().then(|| data.into()),
        page_sources: page_sources.into(),
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
