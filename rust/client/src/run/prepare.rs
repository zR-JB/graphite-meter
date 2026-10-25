//! Path checks: the catalog at `--url`, each selected server's preflight, then throughput and latency paths, by one deadline.
use super::select;
use crate::{
    VERSION,
    config::{Config, PrepKey},
    events::Event,
    format,
    model::Failure,
    net::{CONTROL_TIMEOUT, Client, Fault, LatencyPath, Request, ThroughputPath},
};
use futures_util::future::join_all;
use graphite_meter_proto::{
    bus::Ping,
    catalog::{ServerCatalog, ServerEntry, ServerId},
    discovery::{
        Capabilities, IpFamily, LatencyTransport, Preflight, Protocol, ThroughputTransport,
    },
    envelope::Envelope,
    lease::{LeaseRequest, UNUSED_BOUND},
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

/// Why a server is refused whose lease holds less than its run would.
const TOO_MANY: &str =
    "the test needs more measurements at once than this server admits for one client";
/// How long a path check may take, the catalog and every server included.
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(12);
/// How long a run may take its check's paths and leases, so one presented a control timeout later still holds.
const REUSE: Duration = UNUSED_BOUND.saturating_sub(CONTROL_TIMEOUT);
/// How long a WebTransport session or a latency bus may take to show that its path works.
const VERIFY: Duration = Duration::from_secs(3);
/// How long a datagram probe waits for its reply before another goes.
const DATAGRAM_REPLY: Duration = Duration::from_millis(750);

pub struct Prepared {
    pub key: PrepKey,
    pub at: Instant,
    pub client: Client,
    pub catalog: Arc<[ServerEntry]>,
    pub servers: Vec<ServerPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPath {
    pub id: ServerId,
    /// Its preflight's name, else its catalog entry's; the location likewise.
    pub name: String,
    pub location: String,
    /// The origin its discovery came from, where it signs in.
    pub origin: Origin,
    pub offered: Option<Capabilities>,
    pub path: Result<Paths, Failure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub throughput: ThroughputPath,
    pub latency: Option<LatencyPath>,
    pub stage_limit: Duration,
    pub idle_rtt: Option<Duration>,
    /// The address family the server saw the throughput check arrive from.
    pub ip_family: IpFamily,
}

impl Drop for Prepared {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.client.leased() {
            let client = self.client.clone();
            runtime.spawn(async move { client.end_leases().await });
        }
    }
}

impl Prepared {
    /// Ends its servers' leases, so the next check's are not refused for the share these hold.
    pub async fn end(self) {
        self.client.end_leases().await;
    }

    pub fn event(&self) -> Event {
        let (servers, catalog) = (self.servers.clone().into(), self.catalog.clone());
        Event::Prepared { servers, catalog }
    }

    /// Whether a run with `key` may take this check: every server prepared, the key equal and the check fresh.
    pub fn reusable(&self, key: &PrepKey, now: Instant) -> bool {
        let fresh = now.saturating_duration_since(self.at) <= REUSE;
        fresh && self.key == *key && self.servers.iter().all(|server| server.path.is_ok())
    }
}

/// Checks the paths `config` selects over `client`, which this check owns; a selected server that fails fails alone.
pub async fn prepare(config: &Config, client: Client) -> Result<Prepared, Failure> {
    let at = Instant::now();
    let deadline = time::Instant::now() + PREPARATION_TIMEOUT;
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| refused(crate::config::NO_SERVER))?;
    let request = Request::new(Method::GET, url, Route::Servers);
    let catalog = client.json(Protocol::Negotiated, request, ServerCatalog::decode);
    let received = time::timeout_at(deadline, catalog)
        .await
        .map_err(|_| late())?;
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
    Ok(Prepared {
        key: config.key(),
        at,
        client,
        catalog,
        servers,
    })
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
    let document = client.json(Protocol::Negotiated, request, |data| Ok(data.to_vec()));
    let document = document.await.map_err(|fault| fault.failure())?;
    // The envelope comes first, so a server of another API version is named whatever the rest of its document holds.
    let envelope = Envelope::preflight(&document);
    if let Some(mismatch) = envelope.and_then(|envelope| envelope.mismatch(&server.name, VERSION)) {
        return Err(Fault::Incompatible(mismatch).failure());
    }
    let preflight = Preflight::decode(&document);
    let preflight = preflight.map_err(|error| Fault::Malformed(error.to_string()).failure())?;
    if !preflight.server.name.is_empty() {
        server.name.clone_from(&preflight.server.name);
    }
    if !preflight.server.location.is_empty() {
        server.location.clone_from(&preflight.server.location);
    }
    server.offered = Some(preflight.capabilities.clone());
    let candidates = select::candidates(&config.paths, entry, &served, &preflight, config.probes())
        .map_err(refused)?;
    if config.uploads() && !preflight.capabilities.upload_checkpoint {
        return Err(refused(
            "this test server lacks receiver checkpoints; upgrade it",
        ));
    }
    let bases = preflight.base_urls();
    let targets: Vec<_> = bases.map(|base| base.resolve(&served).clone()).collect();
    client.enroll(&served, &targets);
    let grant = ceilings(config, &candidates.throughput);
    let leased = client.lease(&served, &targets, grant).await;
    let leased = leased.map_err(|fault| fault.failure())?;
    if leased.measurements < grant.measurements || leased.sessions < grant.sessions {
        return Err(Failure::new(FailureReason::ClientLimit, TOO_MANY));
    }
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
    let checked = |path: ThroughputPath| async move { check_throughput(client, &path).await };
    let throughput = first(&candidates.throughput, checked);
    let joined = tokio::try_join!(throughput, latency).map_err(unproven)?;
    let ((throughput, ip_family), latency) = joined;
    let (latency, idle_rtt) = latency.unzip();
    Ok(Paths {
        throughput,
        latency,
        idle_rtt,
        stage_limit: preflight.capabilities.stage_limit(),
        ip_family,
    })
}

/// The first of `candidates` whose check passes, moving on unless a server asks for sign-in; else the first port of
/// another API version, which only an update clears, or the last fault.
async fn first<P: Clone, T, F>(candidates: &[P], check: impl Fn(P) -> F) -> Result<T, Fault>
where
    F: Future<Output = Result<T, Fault>>,
{
    let (mut last, mut incompatible) = (Fault::Malformed("no path to check".into()), None);
    for candidate in candidates {
        match check(candidate.clone()).await {
            Err(Fault::Incompatible(text)) if incompatible.is_none() => incompatible = Some(text),
            Err(fault @ Fault::SignIn(_)) => return Err(fault),
            Err(fault) => last = fault,
            passed => return passed,
        }
    }
    Err(incompatible.map_or(last, Fault::Incompatible))
}

/// A throughput path answering a probe, its WebTransport session opened first; the probe resolves the protocol.
async fn check_throughput(
    client: &Client,
    path: &ThroughputPath,
) -> Result<(ThroughputPath, IpFamily), Fault> {
    if path.transport == ThroughputTransport::WebTransport {
        let session = client.session(
            None,
            &path.origin,
            Route::WtDownload,
            vec![("bytes", "0".into())],
        );
        let opened = timeout(VERIFY, session).await;
        opened.unwrap_or(Err(Fault::TimedOut("WebTransport session")))?;
    }
    let (protocol, ip_family) = client.probe(&path.origin, path.protocol).await?;
    Ok((
        ThroughputPath {
            protocol,
            ..path.clone()
        },
        ip_family,
    ))
}

/// A probe's round trip over its own bus; only the latest reply counts, and a datagram probe repeats unanswered.
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

/// The most any candidate path's run holds at once: both directions' lanes, or a WebTransport session each, beside the
/// latency bus, an upload's feed and its finalize.
fn ceilings(config: &Config, paths: &[ThroughputPath]) -> LeaseRequest {
    let need = |path: &ThroughputPath| match path.transport {
        ThroughputTransport::FetchStream => {
            let lanes = config.lanes(path.protocol, path.transport);
            (lanes.down + lanes.up, 0)
        }
        _ => (2, 2),
    };
    let (lanes, sessions) = paths
        .iter()
        .map(need)
        .fold((0, 0), |(lanes, sessions), (more, also)| {
            (lanes.max(more), sessions.max(also))
        });
    LeaseRequest {
        measurements: u32::try_from(lanes + 3).unwrap_or(u32::MAX),
        sessions,
        uploads: 0,
    }
}

fn refused(text: impl Into<String>) -> Failure {
    Failure::new(FailureReason::PathFailed, text)
}

/// A check that lapsed proved no channel, so it never stopped delivering.
fn unproven(fault: Fault) -> Failure {
    let failure = fault.failure();
    match failure.reason {
        FailureReason::Timeout => refused(failure.text),
        _ => failure,
    }
}

fn late() -> Failure {
    let text = format!(
        "the path check did not finish within {}",
        format::stage_time(PREPARATION_TIMEOUT)
    );
    refused(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_port_of_another_release_is_named_whichever_candidate_it_is() {
        let other = || Fault::Incompatible("S2 runs Graphite Meter 0.15.0".into());
        for order in [[true, false], [false, true]] {
            let found = first(&order, |incompatible| async move {
                Err::<(), _>(if incompatible {
                    other()
                } else {
                    Fault::Malformed("lost".into())
                })
            });
            assert!(
                matches!(found.await, Err(Fault::Incompatible(_))),
                "{order:?}"
            );
        }
    }
}
