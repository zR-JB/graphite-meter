//! Path checks: the catalogue at `--url`, each selected server's preflight, then throughput and latency paths, in 12 s.
use super::select;
use crate::{
    config::{Config, PrepKey},
    events::Event,
    model::Failure,
    net::{Client, Fault, LatencyPath, Request, ThroughputPath},
};
use futures_util::future::join_all;
use graphite_meter_proto::{
    bus::Ping,
    catalog::{ServerCatalog, ServerEntry, ServerId},
    discovery::{Capabilities, LatencyTransport, Preflight, Protocol, ThroughputTransport},
    origin::Origin,
    reason::FailureReason,
    route::Route,
};
use http::Method;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::time::{self, timeout};

/// How long a path check may take, the catalogue and every server included.
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(12);
/// How long a run may take its check's paths and connections.
const REUSE: Duration = Duration::from_secs(30);
/// How long a WebTransport session or a latency bus may take to show that its path works.
const VERIFY: Duration = Duration::from_secs(3);
/// How long a datagram probe waits for its reply before another goes.
const DATAGRAM_REPLY: Duration = Duration::from_millis(750);

/// A path check: its dependencies, start, network state, the catalogue and each selected server's paths.
pub struct Prepared {
    pub key: PrepKey,
    pub at: Instant,
    pub client: Client,
    pub catalogue: Arc<[ServerEntry]>,
    /// In catalogue order.
    pub servers: Vec<ServerPath>,
}

/// A selected server and what its check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPath {
    pub id: ServerId,
    /// Its preflight's name, else its catalogue entry's; the location likewise.
    pub name: String,
    pub location: String,
    /// The origin its discovery came from, where it signs in.
    pub origin: Origin,
    /// What its preflight offered, once it answered.
    pub offered: Option<Capabilities>,
    pub path: Result<Paths, Failure>,
}

/// A prepared server's paths and what its checks measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub throughput: ThroughputPath,
    /// The protocol its throughput check went over, unresolved; upload control requests take it and its connection.
    pub control: Protocol,
    /// When a stage probes latency.
    pub latency: Option<LatencyPath>,
    pub stage_limit: Duration,
    /// The latency check's round trip; zero without one.
    pub idle_rtt: Duration,
}

impl Prepared {
    /// The event that tells viewers what this check found.
    pub fn event(&self) -> Event {
        let (servers, catalogue) = (self.servers.clone().into(), self.catalogue.clone());
        Event::Prepared { servers, catalogue }
    }

    /// Whether a run with `key` may take this check: every server prepared, the key equal and at most 30 s old.
    pub fn reusable(&self, key: &PrepKey, now: Instant) -> bool {
        let fresh = now.saturating_duration_since(self.at) <= REUSE;
        fresh && self.key == *key && self.servers.iter().all(|server| server.path.is_ok())
    }
}

/// Checks the paths `config` selects over `client`, which this check owns; a selected server that fails fails alone.
pub async fn prepare(config: &Config, client: Client) -> Result<Prepared, Failure> {
    let at = Instant::now();
    let deadline = time::Instant::now() + PREPARATION_TIMEOUT;
    let request = Request::new(Method::GET, &config.url, Route::Servers);
    let catalogue = client.json(Protocol::Negotiated, request, ServerCatalog::decode);
    let received = time::timeout_at(deadline, catalogue).await.map_err(|_| late())?;
    let received = received.map_err(|fault| fault.failure())?;
    let selected = select::servers(&received, config).map_err(refused)?;
    let checks = selected.into_iter().map(|entry| async {
        let mut server = ServerPath {
            id: entry.id.clone(),
            name: entry.name.clone(),
            location: entry.location.clone(),
            origin: entry.url.resolve(&config.url).clone(),
            offered: None,
            path: Err(late()),
        };
        let checked = time::timeout_at(deadline, check(&client, config, entry, &mut server)).await;
        server.path = checked.unwrap_or_else(|_| Err(late()));
        server
    });
    let servers = join_all(checks).await;
    let catalogue = received.catalog.servers.into();
    Ok(Prepared { key: config.key(), at, client, catalogue, servers })
}

/// One server's preflight and path checks; its name and location follow its preflight.
async fn check(
    client: &Client,
    config: &Config,
    entry: &ServerEntry,
    server: &mut ServerPath,
) -> Result<Paths, Failure> {
    let served = server.origin.clone();
    let request = Request::new(Method::GET, &served, Route::Preflight);
    let preflight = client.json(Protocol::Negotiated, request, Preflight::decode).await;
    let preflight = preflight.map_err(|fault| fault.failure())?;
    if !preflight.server.name.is_empty() {
        server.name.clone_from(&preflight.server.name);
    }
    if !preflight.server.location.is_empty() {
        server.location.clone_from(&preflight.server.location);
    }
    server.offered = Some(preflight.capabilities.clone());
    let candidates = select::candidates(&config.paths, entry, &served, &preflight, config.probes()).map_err(refused)?;
    if config.uploads() && !preflight.capabilities.upload_checkpoint {
        return Err(refused("receiver checkpoint support is required; upgrade this measurement server"));
    }
    let targets: Vec<_> = preflight
        .base_urls()
        .map(|base| base.resolve(&served).clone())
        .collect();
    client.enroll(&served, &targets);
    let latency = async {
        if candidates.latency.is_empty() {
            return Ok(None);
        }
        let checked = |path: LatencyPath| async move {
            let rtt = check_latency(client, &path).await?;
            Ok(Some((path, rtt)))
        };
        first(&candidates.latency, checked).await
    };
    let checked = |path: ThroughputPath| async move { Ok((path.protocol, check_throughput(client, &path).await?)) };
    let throughput = first(&candidates.throughput, checked);
    let ((control, throughput), latency) = tokio::try_join!(throughput, latency).map_err(|fault| fault.failure())?;
    let (latency, idle_rtt) = latency.unzip();
    Ok(Paths {
        throughput,
        control,
        latency,
        idle_rtt: idle_rtt.unwrap_or_default(),
        stage_limit: preflight.capabilities.stage_limit(),
    })
}

/// The first of `candidates` whose check passes, moving on unless a server asks for sign-in; else the last fault.
async fn first<P: Clone, T, F>(candidates: &[P], check: impl Fn(P) -> F) -> Result<T, Fault>
where
    F: Future<Output = Result<T, Fault>>,
{
    let mut result = Err(Fault::Malformed("no path to check".into()));
    for candidate in candidates {
        result = check(candidate.clone()).await;
        if matches!(result, Ok(_) | Err(Fault::SignIn(_))) {
            break;
        }
    }
    result
}

/// A throughput path answering a probe, its WebTransport session opened first; the probe resolves the protocol.
async fn check_throughput(client: &Client, path: &ThroughputPath) -> Result<ThroughputPath, Fault> {
    if path.transport == ThroughputTransport::WebTransport {
        let session = client.session(&path.origin, Route::WtDownload, vec![("bytes", "0".into())]);
        let opened = timeout(VERIFY, session).await;
        opened.unwrap_or(Err(Fault::TimedOut("WebTransport session")))?;
    }
    let protocol = client.probe(&path.origin, path.protocol).await?;
    Ok(ThroughputPath { protocol, ..path.clone() })
}

/// A probe's round trip over its own bus; only the latest reply counts, and a datagram probe repeats after 750 ms.
async fn check_latency(client: &Client, path: &LatencyPath) -> Result<Duration, Fault> {
    let wait = match path.transport {
        LatencyTransport::WebTransport => DATAGRAM_REPLY,
        LatencyTransport::WebSocket => VERIFY,
    };
    let verified = async {
        let mut bus = client.bus(path).await?;
        let mut ping = Ping { id: 0 };
        loop {
            let sent = Instant::now();
            bus.send(ping).await?;
            let reply = async {
                while bus.next().await?.id != ping.id {}
                Ok(sent.elapsed())
            };
            if let Ok(rtt) = timeout(wait, reply).await {
                return rtt;
            }
            ping = ping.next();
        }
    };
    let verified = timeout(VERIFY, verified).await;
    verified.unwrap_or(Err(Fault::TimedOut("latency reply")))
}

fn refused(text: impl Into<String>) -> Failure {
    Failure::new(FailureReason::PreparationFailed, text)
}

fn late() -> Failure {
    Failure::new(FailureReason::Timeout, "the path check did not finish within 12 seconds")
}
