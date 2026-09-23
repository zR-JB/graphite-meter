//! Select advertised endpoints without inventing authority or choosing an
//! arbitrary server when the operator exposes multiple distinct origins.
use crate::{Error, config::Config};
use graphite_meter_core::{
    catalog::{ServerCatalog, ServerEntry},
    discovery::{
        LatencyTarget, LatencyTransport, Preflight, Protocol, ThroughputTarget, ThroughputTransport,
    },
    origin::canonical_origin,
};

pub fn servers<'a>(
    catalog: &'a ServerCatalog,
    config: &Config,
) -> Result<Vec<&'a ServerEntry>, Error> {
    catalog.validate()?;
    let ids = if config.servers.is_empty() {
        &catalog.default_selection
    } else {
        &config.servers
    };
    catalog.validate_selection(ids)?;
    ids.iter()
        .map(|id| {
            catalog
                .servers
                .iter()
                .find(|entry| entry.id == *id)
                .ok_or_else(|| "selected server is absent from catalogue".into())
        })
        .collect()
}

pub fn throughput(
    config: &Config,
    entry: &ServerEntry,
    preflight: &Preflight,
) -> Result<ThroughputTarget, Error> {
    entry.validate_discovery(preflight)?;
    let order = config.throughput_transport.map_or_else(
        || {
            vec![
                ThroughputTransport::FetchStream,
                ThroughputTransport::WebTransport,
            ]
        },
        |transport| vec![transport],
    );
    for transport in order {
        let candidates = preflight
            .capabilities
            .throughput
            .iter()
            .filter(|target| {
                target.transport == transport
                    && config.throughput_protocol.is_none_or(|protocol| {
                        target.protocol == Protocol::Negotiated || target.protocol == protocol
                    })
            })
            .collect::<Vec<_>>();
        if let Some(target) = choose(
            &candidates,
            config.throughput_origin.as_deref(),
            &entry.url,
            |target| &target.base_url,
        )? {
            let mut target = (*target).clone();
            if target.protocol == Protocol::Negotiated {
                target.protocol = config.throughput_protocol.unwrap_or(Protocol::Negotiated);
            }
            if target.transport != ThroughputTransport::FetchStream
                && target.protocol != Protocol::Http3
            {
                return Err("WebTransport requires HTTP/3".into());
            }
            return Ok(target);
        }
    }
    Err("selected throughput endpoint is not advertised".into())
}

pub fn latency(
    config: &Config,
    entry: &ServerEntry,
    preflight: &Preflight,
) -> Result<LatencyTarget, Error> {
    entry.validate_discovery(preflight)?;
    let order = config.latency_transport.map_or_else(
        || vec![LatencyTransport::WebTransport, LatencyTransport::WebSocket],
        |transport| vec![transport],
    );
    for transport in order {
        let candidates = preflight
            .capabilities
            .latency
            .iter()
            .filter(|target| target.transport == transport)
            .collect::<Vec<_>>();
        if let Some(target) = choose(
            &candidates,
            config.latency_origin.as_deref(),
            &entry.url,
            |target| &target.base_url,
        )? {
            if transport == LatencyTransport::WebTransport
                && config.ping_interval > std::time::Duration::from_secs(15)
            {
                return Err("WebTransport ping interval must not exceed 15 seconds".into());
            }
            return Ok((*target).clone());
        }
    }
    Err("selected latency endpoint is not advertised".into())
}

fn choose<'a, T>(
    candidates: &[&'a T],
    selected: Option<&str>,
    base: &str,
    origin: impl Fn(&T) -> &str,
) -> Result<Option<&'a T>, Error> {
    let wanted = canonical_origin(selected.unwrap_or(base))?;
    for &candidate in candidates {
        if canonical_origin(origin(candidate))? == wanted {
            return Ok(Some(candidate));
        }
    }
    if selected.is_some() {
        return Ok(None);
    }
    match candidates {
        [] => Ok(None),
        [only] => Ok(Some(*only)),
        _ => Err("multiple endpoints available; select an origin explicitly".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_core::discovery::{Capabilities, ServerInfo};

    #[test]
    fn explicit_origin_cannot_override_protocol_or_discovery_authority() {
        let entry = ServerEntry {
            id: "self".into(),
            url: "https://meter.example".into(),
            ..Default::default()
        };
        let mut preflight = Preflight {
            server: ServerInfo::default(),
            engine_version: String::new(),
            generation: "one".into(),
            capabilities: Capabilities {
                upload_checkpoint: true,
                latency: vec![],
                throughput: vec![ThroughputTarget {
                    base_url: entry.url.clone(),
                    transport: ThroughputTransport::FetchStream,
                    protocol: Protocol::Http2,
                }],
            },
        };
        let config = Config {
            throughput_origin: Some(entry.url.clone()),
            throughput_protocol: Some(Protocol::Http3),
            ..Default::default()
        };
        assert!(throughput(&config, &entry, &preflight).is_err());
        preflight.capabilities.throughput[0].protocol = Protocol::Negotiated;
        assert_eq!(
            throughput(&config, &entry, &preflight).unwrap().protocol,
            Protocol::Http3
        );
        preflight.capabilities.throughput[0].base_url = "https://foreign.example".into();
        assert!(throughput(&config, &entry, &preflight).is_err());
    }

    #[test]
    fn selection_uses_selected_servers_only() {
        let mut catalog = ServerCatalog::singleton().resolve("https://meter.example");
        catalog.servers.push(ServerEntry {
            id: "peer".into(),
            url: "https://peer.example".into(),
            ..Default::default()
        });
        let mut config = Config {
            streams: 128,
            ..Default::default()
        };
        assert_eq!(servers(&catalog, &config).unwrap().len(), 1);
        config.servers = vec!["self".into(), "peer".into()];
        assert_eq!(servers(&catalog, &config).unwrap().len(), 2);
    }
}
