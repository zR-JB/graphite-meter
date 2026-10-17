//! Checks the catalog at `--url`, each selected server's preflight, then its throughput and latency paths, in 12 s.
use super::select;
use crate::{
    config::{Config, PrepKey},
    events::Event,
    format,
    model::Failure,
    net::{Client, Fault, LatencyPath, Request, ThroughputPath},
};
use futures_util::future::join_all;
use graphite_meter_proto::{
    bus::Ping,
    catalog::{ServerCatalog, ServerEntry, ServerId},
    discovery::{Capabilities, IpFamily, LatencyTransport, Preflight, Protocol, ThroughputTransport},
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

/// A path check may take this long, the catalog and every server included.
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(12);
/// A run may take its check's paths and connections for this long.
const REUSE: Duration = Duration::from_secs(30);
/// A WebTransport session or a latency bus may take this long to show that its path works.
const VERIFY: Duration = Duration::from_secs(3);
/// A datagram probe waits this long for its reply before another goes.
const DATAGRAM_REPLY: Duration = Duration::from_millis(750);

/// A path check records its dependencies, start, network state, the catalog and each selected server's paths.
pub struct Prepared {
    pub key: PrepKey,
    pub at: Instant,
    pub client: Client,
    pub catalog: Arc<[ServerEntry]>,
    /// The servers come in catalog order.
    pub servers: Vec<ServerPath>,
}

/// Pairs a selected server with what its check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPath {
    pub id: ServerId,
    /// Its name comes from its preflight, else its catalog entry, and the location likewise.
    pub name: String,
    pub location: String,
    /// Its discovery came from this origin, where it signs in.
    pub origin: Origin,
    /// Its preflight offered this, once it answered.
    pub offered: Option<Capabilities>,
    pub path: Result<Paths, Failure>,
}

/// Holds a prepared server's paths and what its checks measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub throughput: ThroughputPath,
    /// Its throughput check went over this protocol, unresolved. Upload control requests take it and its connection.
    pub control: Protocol,
    /// A stage that probes latency uses this path.
    pub latency: Option<LatencyPath>,
    pub stage_limit: Duration,
    /// The latency check measured this round trip.
    pub idle_rtt: Option<Duration>,
    /// The server saw the throughput check arrive from this address family.
    pub ip_family: IpFamily,
}

impl Prepared {
    /// Builds the event that tells viewers what this check found.
    pub fn event(&self) -> Event {
        let (servers, catalog) = (self.servers.clone().into(), self.catalog.clone());
        Event::Prepared { servers, catalog }
    }

    /// A run with `key` may take this check when every server prepared, the key is equal and it is at most 30 s old.
    pub fn reusable(&self, key: &PrepKey, now: Instant) -> bool {
        let fresh = now.saturating_duration_since(self.at) <= REUSE;
        fresh && self.key == *key && self.servers.iter().all(|server| server.path.is_ok())
    }
}

/// Checks the paths `config` selects over `client`, which this check owns; a selected server that fails fails alone.
pub async fn prepare(config: &Config, client: Client) -> Result<Prepared, Failure> {
    let at = Instant::now();
    let deadline = time::Instant::now() + PREPARATION_TIMEOUT;
    let url = config.url.as_ref().ok_or_else(|| refused(crate::config::NO_SERVER))?;
    let request = Request::new(Method::GET, url, Route::Servers);
    let catalog = client.json(Protocol::Negotiated, request, ServerCatalog::decode);
    let received = time::timeout_at(deadline, catalog).await.map_err(|_| late())?;
    let received = received.map_err(|fault| fault.failure())?;
    let selected = select::servers(&received, config).map_err(refused)?;
    let checks = selected.into_iter().map(|entry| async {
        let mut server = ServerPath {
            id: entry.id.clone(),
            name: entry.name.clone(),
            location: entry.location.clone(),
            origin: entry.url.resolve(url).clone(),
            offered: None,
            path: Err(late()),
        };
        let checked = time::timeout_at(deadline, check(&client, config, entry, &mut server)).await;
        server.path = checked.unwrap_or_else(|_| Err(late()));
        server
    });
    let servers = join_all(checks).await;
    let catalog = received.catalog.servers.into();
    Ok(Prepared { key: config.key(), at, client, catalog, servers })
}

/// Runs one server's preflight and path checks. Its name and location follow its preflight.
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
        return Err(refused("this test server lacks receiver checkpoints; upgrade it"));
    }
    let bases = preflight.base_urls();
    let targets: Vec<_> = bases.map(|base| base.resolve(&served).clone()).collect();
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
    let joined = tokio::try_join!(throughput, latency).map_err(|fault| fault.failure())?;
    let ((control, (throughput, ip_family)), latency) = joined;
    let (latency, idle_rtt) = latency.unzip();
    Ok(Paths {
        throughput,
        control,
        latency,
        idle_rtt,
        stage_limit: preflight.capabilities.stage_limit(),
        ip_family,
    })
}

/// Finds the first of `candidates` whose check passes, moving on unless a server asks for sign-in, else the last fault.
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

/// Checks a throughput path answers a probe, its WebTransport session opened first. The probe resolves the protocol
/// and names the address family it arrived from.
async fn check_throughput(client: &Client, path: &ThroughputPath) -> Result<(ThroughputPath, IpFamily), Fault> {
    if path.transport == ThroughputTransport::WebTransport {
        let session = client.session(None, &path.origin, Route::WtDownload, vec![("bytes", "0".into())]);
        let opened = timeout(VERIFY, session).await;
        opened.unwrap_or(Err(Fault::TimedOut("WebTransport session")))?;
    }
    let (protocol, ip_family) = client.probe(&path.origin, path.protocol).await?;
    Ok((ThroughputPath { protocol, ..path.clone() }, ip_family))
}

/// Times a probe's round trip over its own bus. Only the latest reply counts. A datagram probe repeats after 750 ms.
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
                loop {
                    let (pong, arrived) = bus.next().await?;
                    if pong.id == ping.id {
                        return Ok(arrived.duration_since(sent));
                    }
                }
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
    let text = format!("the path check did not finish within {}", format::stage_time(PREPARATION_TIMEOUT));
    Failure::new(FailureReason::Timeout, text)
}
