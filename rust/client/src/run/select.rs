//! Which servers a run takes, the paths to check on each in the order to try them, and the stage limit they share.
use super::prepare::ServerPath;
use crate::{
    config::{Config, PathChoice},
    model::Stage,
    net::{LatencyPath, ThroughputPath},
};
use graphite_meter_proto::{
    catalog::{Received, ServerEntry, ServerId},
    discovery::{LatencyTransport, Preflight, Protocol, ThroughputTarget, ThroughputTransport},
    duration,
    origin::{BaseUrl, Origin},
    text::quote,
};
use std::time::Duration;

/// A server's paths, each list in the order to check them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub throughput: Vec<ThroughputPath>,
    pub latency: Vec<LatencyPath>,
}

/// The selected entries in catalogue order: the `--server` IDs, else the operator's default.
pub fn servers<'a>(received: &'a Received, config: &Config) -> Result<Vec<&'a ServerEntry>, String> {
    let chosen = |id: &str| config.servers.iter().any(|server| server.as_str() == id);
    if let Some(left) = received.rejected.iter().find(|rejected| chosen(&rejected.id)) {
        return Err(format!("the catalogue's server {} was left out: {}", quote(&left.id), left.error));
    }
    let catalog = &received.catalog;
    let ids = match config.servers.is_empty() {
        true => &catalog.default_selection,
        false => &config.servers,
    };
    catalog.validate_selection(ids).map_err(|error| {
        let known = |id: &ServerId| catalog.servers.iter().any(|entry| entry.id == *id);
        let unknown = ids.iter().find(|id| !known(id));
        unknown.map_or_else(|| error.to_string(), |id| format!("unknown or repeated server {}", quote(id.as_str())))
    })?;
    let explicit = config.paths.throughput_origin.is_some() || config.paths.latency_origin.is_some();
    if explicit && ids.len() > 1 {
        return Err("explicit origins need a single selected server; use Automatic origins for several".into());
    }
    Ok(catalog.servers.iter().filter(|entry| ids.contains(&entry.id)).collect())
}

/// Paths on `entry`'s server: throughput by fetch-stream then WebTransport, latency by WebTransport then WebSocket.
pub fn candidates(
    choice: &PathChoice,
    entry: &ServerEntry,
    served: &Origin,
    preflight: &Preflight,
    latency: bool,
) -> Result<Candidates, String> {
    if !entry.approves(served, preflight) {
        return Err(format!("server {} advertised an unapproved target origin", quote(entry.id.as_str())));
    }
    let offered = &preflight.capabilities;
    let transports = choice.throughput_transport.map_or_else(
        || vec![ThroughputTransport::FetchStream, ThroughputTransport::WebTransport],
        |forced| vec![forced],
    );
    let wanted = choice.throughput_origin.as_ref();
    let throughput = resolved(transports.into_iter().map(|transport| {
        let eligible = offered.throughput.iter().filter(|target| {
            target.transport == transport && (wanted.is_some() || serves(target.protocol, choice.protocol))
        });
        let target = pick("throughput", eligible, wanted, served, |target| &target.base_url)?;
        throughput_path(target, choice.protocol, served)
    }))?;
    let transports = match (latency, choice.latency_transport) {
        (false, _) => vec![],
        (true, None) => vec![LatencyTransport::WebTransport, LatencyTransport::WebSocket],
        (true, Some(forced)) => vec![forced],
    };
    let latency = resolved(transports.into_iter().map(|transport| {
        let eligible = offered.latency.iter().filter(|target| target.transport == transport);
        let target = pick("latency", eligible, choice.latency_origin.as_ref(), served, |target| &target.base_url)?;
        Ok(LatencyPath { origin: target.base_url.resolve(served).clone(), transport })
    }))?;
    Ok(Candidates { throughput, latency })
}

/// Refuses a planned stage longer than the smallest limit among the prepared servers, naming the server that sets it.
pub fn fit(plan: &[(Stage, Duration)], servers: &[ServerPath]) -> Result<(), String> {
    let limits = servers
        .iter()
        .filter_map(|server| Some((server.path.as_ref().ok()?.stage_limit, &server.name)));
    let Some((limit, name)) = limits.min_by_key(|(limit, _)| *limit) else {
        return Ok(());
    };
    let Some((stage, _)) = plan.iter().find(|(_, duration)| *duration > limit) else {
        return Ok(());
    };
    Err(format!("{name} allows stages up to {}; shorten the {} stage", short(limit), stage.name()))
}

/// The paths that resolved, in order; the first refusal when none did.
fn resolved<T>(results: impl Iterator<Item = Result<T, String>>) -> Result<Vec<T>, String> {
    let (mut found, mut refused) = (Vec::new(), None);
    for result in results {
        match result {
            Ok(path) => found.push(path),
            Err(refusal) => refused = refused.or(Some(refusal)),
        }
    }
    refused.filter(|_| found.is_empty()).map_or(Ok(found), Err)
}

/// The target at `wanted`; with an automatic origin the one at `served`, else the only one.
fn pick<'a, T>(
    kind: &str,
    eligible: impl Iterator<Item = &'a T>,
    wanted: Option<&Origin>,
    served: &Origin,
    base: fn(&T) -> &BaseUrl,
) -> Result<&'a T, String> {
    let mut others = Vec::new();
    for target in eligible {
        if base(target).resolve(served) == wanted.unwrap_or(served) {
            return Ok(target);
        }
        others.push(target);
    }
    match (wanted, others.as_slice()) {
        (None, [only]) => Ok(only),
        (None, [_, _, ..]) => Err(format!("several {kind} targets are available; select an origin")),
        _ => {
            let wanted = wanted.map_or_else(|| "auto".into(), Origin::to_string);
            Err(format!("{kind} target {} unavailable", quote(&wanted)))
        }
    }
}

/// Whether a target of `offered` protocol serves a `forced` one: as it, or negotiated.
fn serves(offered: Protocol, forced: Option<Protocol>) -> bool {
    forced.is_none_or(|forced| offered == forced || offered == Protocol::Negotiated)
}

/// `target` as a path; a forced protocol replaces a negotiated one and refuses a fixed other.
fn throughput_path(
    target: &ThroughputTarget,
    forced: Option<Protocol>,
    served: &Origin,
) -> Result<ThroughputPath, String> {
    let protocol = match forced {
        Some(forced) if target.protocol != Protocol::Negotiated && target.protocol != forced => {
            return Err(format!("endpoint is fixed to {}, cannot use {}", target.protocol.name(), forced.name()));
        }
        forced => forced.unwrap_or(target.protocol),
    };
    if target.transport == ThroughputTransport::WebTransport && protocol != Protocol::Http3 {
        return Err("WebTransport requires HTTP/3".into());
    }
    let origin = target.base_url.resolve(served).clone();
    Ok(ThroughputPath { origin, transport: target.transport, protocol })
}

/// A stage limit as an operator writes it, such as `5m` or `1h30m`.
fn short(limit: Duration) -> String {
    let mut text = duration::format(limit);
    for whole in ["m0s", "h0m"] {
        if text.ends_with(whole) {
            text.truncate(text.len() - 2);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Parsed, parse},
        model::Failure,
        net::Client,
        run::prepare::{Paths, Prepared},
    };
    use graphite_meter_net::Pool;
    use graphite_meter_proto::{catalog::ServerCatalog, reason::FailureReason};
    use std::{ffi::OsString, sync::Arc, time::Instant};

    const CATALOGUE: &str = r#"{"defaultSelection": ["b"], "servers": [
        {"id": "self", "url": ".", "name": "Self"},
        {"id": "a", "url": "https://a.example/", "name": "A"},
        {"id": "b", "url": "https://b.example", "name": "B"},
        {"id": "broken", "url": "https://broken.example/path", "name": "Broken"}]}"#;

    fn config(args: &str) -> Config {
        match parse(args.split_whitespace().map(OsString::from)) {
            Ok(Parsed::Run(config)) => *config,
            other => panic!("{args}: {other:?}"),
        }
    }

    fn selected(args: &str) -> Result<Vec<String>, String> {
        let received = ServerCatalog::decode(CATALOGUE.as_bytes()).unwrap();
        let servers = servers(&received, &config(args))?;
        Ok(servers.iter().map(|entry| entry.id.to_string()).collect())
    }

    #[test]
    fn the_selection_is_the_server_ids_or_the_default_in_catalogue_order() {
        assert_eq!(selected("").unwrap(), ["b"]);
        assert_eq!(selected("-server b -server self").unwrap(), ["self", "b"]);
        assert_eq!(selected("-server b -server a -server self").unwrap(), ["self", "a", "b"]);
        let unknown = "unknown or repeated server \"nowhere\"";
        assert_eq!(selected("-server a -server nowhere").unwrap_err(), unknown);
        let left = "the catalogue's server \"broken\" was left out: invalid catalogue origin";
        assert_eq!(selected("-server broken").unwrap_err(), left);
        let explicit = "explicit origins need a single selected server; use Automatic origins for several";
        assert_eq!(selected("-server a -server b -latency-origin https://a.example").unwrap_err(), explicit);
        assert_eq!(selected("-throughput-origin https://b.example").unwrap(), ["b"]);
    }

    const SERVED: &str = "http://meter.example:7246";

    fn preflight(throughput: &[(&str, &str, &str)], latency: &[(&str, &str)]) -> Preflight {
        use serde_json::json;
        let fetch =
            |(base, transport, protocol)| json!({"baseUrl": base, "transport": transport, "protocol": protocol});
        let throughput: Vec<_> = throughput.iter().copied().map(fetch).collect();
        let probe = |(base, transport)| json!({"baseUrl": base, "transport": transport});
        let latency: Vec<_> = latency.iter().copied().map(probe).collect();
        let capabilities = json!({"throughput": throughput, "latency": latency});
        let document = json!({"server": {"name": "meter"}, "engineVersion": "1", "generation": "g",
            "capabilities": capabilities});
        Preflight::decode(document.to_string().as_bytes()).unwrap()
    }

    fn native() -> Preflight {
        let throughput = [
            (SERVED, "fetch-stream", "http1"),
            ("https://meter.example:7248", "fetch-stream", "http2"),
            ("https://meter.example:7249", "fetch-stream", "http3"),
            ("https://meter.example:7249", "webtransport", "http3"),
            ("https://meter.example:7249", "webtransport-datagram", "http3"),
        ];
        preflight(&throughput, &[(SERVED, "websocket"), ("https://meter.example:7249", "webtransport")])
    }

    /// The candidates as `transport protocol port` and `transport port` lines.
    fn paths(args: &str, served: &str, preflight: &Preflight, entry: &ServerEntry) -> Result<String, String> {
        let config = config(args);
        let served = Origin::parse(served).unwrap();
        let found = candidates(&config.paths, entry, &served, preflight, config.probes())?;
        let fetch =
            |path: &ThroughputPath| format!("{} {} {}", path.transport.name(), path.protocol.name(), path.origin.port);
        let probe = |path: &LatencyPath| format!("{} {}", path.transport.name(), path.origin.port);
        let (throughput, latency) = (found.throughput.iter().map(fetch), found.latency.iter().map(probe));
        Ok(throughput.chain(latency).collect::<Vec<_>>().join(", "))
    }

    fn own() -> ServerEntry {
        ServerCatalog::singleton().servers.remove(0)
    }

    #[test]
    fn automatic_paths_follow_the_native_order_and_forced_ones_never_downgrade() {
        let (native, own) = (native(), own());
        #[rustfmt::skip]
        let rows = [
            ("", "fetch-stream http1 7246, webtransport http3 7249, webtransport 7249, websocket 7246"),
            ("-throughput-transport webtransport -latency-transport websocket",
                "webtransport http3 7249, websocket 7246"),
            ("-throughput-origin https://meter.example:7248 -stages down -loaded-latency=false",
                "fetch-stream http2 7248"),
            ("-throughput-protocol http3 -stages up -loaded-latency=false",
                "fetch-stream http3 7249, webtransport http3 7249"),
            ("-latency-transport webtransport -stages ping",
                "fetch-stream http1 7246, webtransport http3 7249, webtransport 7249"),
        ];
        for (args, expected) in rows {
            assert_eq!(paths(args, SERVED, &native, &own).unwrap(), expected, "{args}");
        }
        #[rustfmt::skip]
        let refusals = [
            ("-throughput-origin https://meter.example:7249 -throughput-protocol http2",
                "endpoint is fixed to http3, cannot use http2"),
            ("-throughput-origin https://other.example", "throughput target \"https://other.example\" unavailable"),
            ("-latency-origin https://other.example", "latency target \"https://other.example\" unavailable"),
        ];
        for (args, refusal) in refusals {
            assert_eq!(paths(args, SERVED, &native, &own).unwrap_err(), refusal, "{args}");
        }
        let elsewhere = |args| paths(args, "https://meter.example", &native, &own);
        assert_eq!(elsewhere("-stages down -loaded-latency=false").unwrap(), "webtransport http3 7249");
        let several = "several throughput targets are available; select an origin";
        assert_eq!(elsewhere("-throughput-transport fetch-stream").unwrap_err(), several);
    }

    /// A server named after its ID in capitals with a stage limit of `seconds`, prepared unless `failure`.
    fn server(name: &str, seconds: u64, failure: Option<Failure>) -> ServerPath {
        let origin = Origin::parse(SERVED).unwrap();
        let (transport, protocol) = (ThroughputTransport::FetchStream, Protocol::Http1);
        let throughput = ThroughputPath { origin: origin.clone(), transport, protocol };
        let (stage_limit, idle_rtt) = (Duration::from_secs(seconds), Duration::ZERO);
        let paths = Paths {
            throughput,
            control: protocol,
            latency: None,
            stage_limit,
            idle_rtt,
        };
        let (id, name) = (ServerId::parse(name).unwrap(), name.to_uppercase());
        let path = failure.map_or(Ok(paths), Err);
        ServerPath {
            id,
            name,
            location: String::new(),
            origin,
            offered: None,
            path,
        }
    }

    #[test]
    fn a_run_reuses_a_whole_check_with_its_key_for_30_s() {
        let at = Instant::now();
        let client = Client::new(false, Arc::new(Pool::inline()));
        let (key, catalogue) = (config("-server a -stages down").key(), Arc::new([]));
        let prepared = Prepared {
            key,
            at,
            client,
            catalogue,
            servers: vec![server("a", 30, None)],
        };
        let longer = config("-server a -stages down -download-duration 1m -warmup 2s").key();
        let reuse = Duration::from_secs(30);
        assert!(prepared.reusable(&longer, at + reuse));
        assert!(!prepared.reusable(&longer, at + reuse + Duration::from_millis(1)));
        assert!(!prepared.reusable(&config("-server a -stages down,up").key(), at));
        assert!(!prepared.reusable(&config("-server a -stages down -insecure").key(), at));
        let late = Some(Failure::new(FailureReason::Timeout, "late"));
        let failed = Prepared {
            servers: vec![server("a", 30, None), server("b", 30, late)],
            ..prepared
        };
        assert!(!failed.reusable(&longer, at));
    }
}
