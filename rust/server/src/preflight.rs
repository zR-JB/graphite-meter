//! Discovery is built once from resolved policy, independent of the listener adapter.
use crate::config::{Config, ConfigError, NativeKind};
use graphite_meter_core::{
    discovery::{
        Capabilities, LatencyTarget, LatencyTransport, Preflight as Document, Protocol, ServerInfo,
        ThroughputTarget, ThroughputTransport,
    },
    origin::{key, target_origin},
};
use std::sync::Arc;

pub struct Preflight {
    config: Arc<Config>,
    generation: String,
}

impl Preflight {
    pub fn new(config: Arc<Config>) -> Result<Self, ConfigError> {
        config.validate()?;
        let mut nonce = [0_u8; 16];
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut nonce)
            .map_err(|_| "failed to generate discovery identity")?;
        let generation = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self { config, generation })
    }

    pub fn build(&self, authority: &str) -> Result<Document, ConfigError> {
        if authority.contains('@') {
            return Err("request authority must not contain credentials".into());
        }
        let authority: http::uri::Authority = authority.parse()?;
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        self.build_for_host(host)
    }

    pub fn build_for_host(&self, host: &str) -> Result<Document, ConfigError> {
        let config = &self.config;
        let mut capabilities = Capabilities {
            upload_checkpoint: true,
            throughput: Vec::new(),
            latency: Vec::new(),
        };
        for kind in NativeKind::ALL {
            if !config.native_advertised(kind) {
                continue;
            }
            let listener = config.listener(kind);
            let base = if listener.public_origin.is_empty() {
                native_origin(kind, host, &listener.address)?
            } else {
                listener.public_origin.clone()
            };
            let protocol = match kind {
                NativeKind::H1 | NativeKind::H1Tls => Protocol::Http1,
                NativeKind::H2 => Protocol::Http2,
                NativeKind::H3 => Protocol::Http3,
            };
            add_throughput(&mut capabilities, &base, protocol);
            if matches!(kind, NativeKind::H1 | NativeKind::H1Tls) {
                add_latency(&mut capabilities, &base);
            }
            if kind == NativeKind::H3 {
                for transport in [
                    ThroughputTransport::WebTransport,
                    ThroughputTransport::WebTransportDatagram,
                ] {
                    capabilities.throughput.push(ThroughputTarget {
                        base_url: base.clone(),
                        transport,
                        protocol: Protocol::Http3,
                    });
                }
                capabilities.latency.push(LatencyTarget {
                    base_url: base,
                    transport: LatencyTransport::WebTransport,
                });
            }
        }
        for raw in &config.public.both {
            add_throughput(&mut capabilities, public_base(raw), Protocol::Negotiated);
            add_latency(&mut capabilities, public_base(raw));
        }
        for raw in &config.public.throughput {
            add_throughput(&mut capabilities, public_base(raw), Protocol::Negotiated);
        }
        for raw in &config.public.latency {
            add_latency(&mut capabilities, public_base(raw));
        }
        let document = Document {
            server: ServerInfo {
                name: config.server_name.clone(),
                location: config.server_location.clone(),
            },
            engine_version: config.engine_version.clone(),
            generation: self.generation.clone(),
            capabilities,
        };
        document.validate()?;
        Ok(document)
    }

    pub fn connect_origins(&self, host: &str) -> Result<Vec<String>, ConfigError> {
        let document = self.build_for_host(host)?;
        let mut origins = Vec::new();
        let mut add = |origin: String| {
            if !origin.is_empty() && origin != "." && !origins.contains(&origin) {
                origins.push(origin);
            }
        };
        for target in document.capabilities.throughput {
            add(target.base_url);
        }
        for target in document.capabilities.latency {
            add(target.base_url.clone());
            if let Some(host) = target.base_url.strip_prefix("https://") {
                add(format!("wss://{host}"));
            } else if let Some(host) = target.base_url.strip_prefix("http://") {
                add(format!("ws://{host}"));
            }
        }
        Ok(origins)
    }
}

fn add_throughput(capabilities: &mut Capabilities, base: &str, protocol: Protocol) {
    let base = base.trim_end_matches('/');
    if let Some(target) = capabilities.throughput.iter_mut().find(|target| {
        target.transport == ThroughputTransport::FetchStream && key(&target.base_url) == key(base)
    }) {
        if target.protocol != protocol {
            target.protocol = Protocol::Negotiated;
        }
    } else {
        capabilities.throughput.push(ThroughputTarget {
            base_url: base.into(),
            transport: ThroughputTransport::FetchStream,
            protocol,
        });
    }
}

fn add_latency(capabilities: &mut Capabilities, base: &str) {
    let base = base.trim_end_matches('/');
    if !capabilities.latency.iter().any(|target| {
        target.transport == LatencyTransport::WebSocket && key(&target.base_url) == key(base)
    }) {
        capabilities.latency.push(LatencyTarget {
            base_url: base.into(),
            transport: LatencyTransport::WebSocket,
        });
    }
}

fn public_base(raw: &str) -> &str {
    if raw == "self" { "." } else { raw }
}

fn native_origin(kind: NativeKind, host: &str, address: &str) -> Result<String, ConfigError> {
    let scheme = if kind == NativeKind::H1 {
        "http"
    } else {
        "https"
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = address
        .rsplit_once(':')
        .map(|(_, port)| port)
        .filter(|port| !port.is_empty());
    let origin = match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    };
    target_origin(&origin)?;
    Ok(origin)
}
