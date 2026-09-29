//! Select advertised endpoints without inventing authority or choosing an
//! arbitrary server when the operator exposes multiple distinct origins.
use crate::{Error, config::Config};
use graphite_meter_core::{
    catalog::{ServerCatalog, ServerEntry},
    discovery::{LatencyTarget, LatencyTransport, Preflight, Protocol, ThroughputTarget, ThroughputTransport},
    origin::canonical_origin,
};

pub fn servers<'a>(catalog: &'a ServerCatalog, config: &Config) -> Result<Vec<&'a ServerEntry>, Error> {
    catalog.validate()?;
    let ids = if config.servers.is_empty() {
        &catalog.default_selection
    } else {
        &config.servers
    };
    catalog.validate_selection(ids)?;
    if ids.len() > 1 && (config.throughput_origin.is_some() || config.latency_origin.is_some()) {
        return Err("explicit origins need a single selected server; use Automatic origins for several".into());
    }
    Ok(catalog.servers.iter().filter(|entry| ids.contains(&entry.id)).collect())
}

pub fn throughput(
    config: &Config,
    entry: &ServerEntry,
    preflight: &Preflight,
    selected: Option<ThroughputTransport>,
) -> Result<ThroughputTarget, Error> {
    let order = selected.map_or_else(
        || vec![ThroughputTransport::FetchStream, ThroughputTransport::WebTransport],
        |transport| vec![transport],
    );
    let mut first_error = None;
    for transport in order {
        match throughput_candidate(config, entry, preflight, transport) {
            Ok(Some(target)) => return Ok(target),
            Ok(None) => {}
            Err(error) if selected.is_some() => return Err(error),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| unavailable("throughput", config.throughput_origin.as_deref())))
}

/// Go's pickTarget refusal: no advertised target matches the selection, which is "auto" unset.
fn unavailable(kind: &str, selected: Option<&str>) -> Error {
    format!("{kind} target {:?} unavailable", selected.unwrap_or("auto")).into()
}

fn throughput_candidate(
    config: &Config,
    entry: &ServerEntry,
    preflight: &Preflight,
    transport: ThroughputTransport,
) -> Result<Option<ThroughputTarget>, Error> {
    if transport == ThroughputTransport::WebTransportDatagram {
        return Err("native throughput supports auto, fetch-stream or webtransport".into());
    }
    entry.validate_discovery(preflight)?;
    let candidates = preflight
        .capabilities
        .throughput
        .iter()
        .filter(|target| {
            target.transport == transport
                && config
                    .throughput_protocol
                    .is_none_or(|protocol| target.protocol == Protocol::Negotiated || target.protocol == protocol)
        })
        .collect::<Vec<_>>();
    let selected = config.throughput_origin.as_deref();
    let Some(target) = choose("throughput", &candidates, selected, &entry.url, |target| {
        &target.base_url
    })?
    else {
        return Ok(None);
    };
    let mut target = (*target).clone();
    if target.protocol == Protocol::Negotiated {
        target.protocol = config.throughput_protocol.unwrap_or(Protocol::Negotiated);
    }
    if target.transport != ThroughputTransport::FetchStream && target.protocol != Protocol::Http3 {
        return Err("WebTransport requires HTTP/3".into());
    }
    Ok(Some(target))
}

pub fn latency(
    config: &Config,
    entry: &ServerEntry,
    preflight: &Preflight,
    selected: Option<LatencyTransport>,
) -> Result<LatencyTarget, Error> {
    entry.validate_discovery(preflight)?;
    let order = selected.map_or_else(
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
        let selected = config.latency_origin.as_deref();
        if let Some(target) = choose("latency", &candidates, selected, &entry.url, |target| &target.base_url)? {
            return Ok((*target).clone());
        }
    }
    Err(unavailable("latency", config.latency_origin.as_deref()))
}

fn choose<'a, T>(
    kind: &str,
    candidates: &[&'a T],
    selected: Option<&str>,
    base: &str,
    origin: impl Fn(&T) -> &str,
) -> Result<Option<&'a T>, Error> {
    // As Go's pickTarget, a malformed selection matches no target.
    let wanted = match selected {
        Some(selected) => canonical_origin(selected).ok(),
        None => Some(canonical_origin(base)?),
    };
    for &candidate in candidates {
        if Some(canonical_origin(origin(candidate))?) == wanted {
            return Ok(Some(candidate));
        }
    }
    if selected.is_some() {
        return Ok(None);
    }
    match candidates {
        [] => Ok(None),
        [only] => Ok(Some(*only)),
        _ => Err(format!("several {kind} targets are available; select an origin").into()),
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
        assert!(throughput(&config, &entry, &preflight, None).is_err());
        preflight.capabilities.throughput[0].protocol = Protocol::Negotiated;
        assert_eq!(
            throughput(&config, &entry, &preflight, None).unwrap().protocol,
            Protocol::Http3
        );
        preflight.capabilities.throughput[0].base_url = "https://foreign.example".into();
        assert!(throughput(&config, &entry, &preflight, None).is_err());
        // As Go's prepareRun (prepare.go:113-115), an explicit origin needs a single server.
        let other = ServerEntry {
            id: "other".into(),
            url: "https://other.example".into(),
            ..Default::default()
        };
        let ids = vec!["self".into(), "other".into()];
        let catalog = ServerCatalog {
            default_selection: ids,
            servers: vec![entry, other],
        };
        let refused = servers(&catalog, &config).err().map(|error| error.to_string());
        let message = "explicit origins need a single selected server; use Automatic origins for several";
        assert_eq!(refused.as_deref(), Some(message));
        let single = Config {
            servers: vec!["self".into()],
            ..config
        };
        assert_eq!(servers(&catalog, &single).map(|selected| selected.len()).ok(), Some(1));
    }
}
