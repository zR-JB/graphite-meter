//! Run preparation and measurement are independent of terminal rendering.
use crate::{
    Error,
    config::Config,
    model::{ServerSummary, Snapshot, Stage},
    net::Http,
    selection,
    transport::Transport,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::route::Route;
use graphite_meter_core::{
    catalog::ServerEntry,
    discovery::{LatencyTarget, LatencyTransport, Probe, Protocol, ThroughputTarget, ThroughputTransport},
};
use http::Method;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

#[cfg(test)]
pub(crate) mod prepare_tests;

const PREPARATION_TIMEOUT: Duration = Duration::from_secs(12);

struct PreparedServer {
    entry: ServerEntry,
    client: Http,
    throughput: Option<ThroughputTarget>,
    http: Option<Arc<Transport>>,
    latency: Option<LatencyTarget>,
    idle_rtt: Duration,
    /// Go's replacedUpload (upload.go:23-31): the run's one replacement upload receiver here.
    replaced_upload: Arc<AtomicBool>,
}

pub struct PreparedRun {
    servers: Vec<PreparedServer>,
    key: Config,
    pub verified_at: Instant,
}

impl PreparedRun {
    pub fn fresh_for(&self, config: &Config) -> bool {
        self.key == config.preparation_key() && self.verified_at.elapsed() <= Duration::from_secs(30)
    }
}

pub async fn prepare_run(
    config: &Config,
    http: &Http,
    snapshots: &watch::Sender<Snapshot>,
) -> Result<PreparedRun, Error> {
    let verified_at = Instant::now();
    let preparation = prepare(config, http, snapshots, Instant::now() + PREPARATION_TIMEOUT).await?;
    if !preparation.failures.is_empty() {
        return Err(preferred(preparation.failures));
    }
    Ok(PreparedRun {
        servers: preparation.servers,
        key: config.preparation_key(),
        verified_at,
    })
}

#[derive(Debug)]
struct ServerError {
    id: String,
    label: String,
    source: Error,
}

struct Preparation {
    servers: Vec<PreparedServer>,
    failures: Vec<ServerError>,
}

/// The controller approves one origin per retry, so prefer a sign-in challenge.
fn preferred(mut failures: Vec<ServerError>) -> Error {
    let index = failures
        .iter()
        .position(|failure| crate::failure::sign_in(failure.source.as_ref()).is_some())
        .unwrap_or(0);
    failures.swap_remove(index).into()
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.label, self.source)
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// One deadline covers the catalogue and every server, so a slow server fails alone.
async fn prepare(
    config: &Config,
    http: &Http,
    snapshots: &watch::Sender<Snapshot>,
    deadline: Instant,
) -> Result<Preparation, Error> {
    let late = || -> Error {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the path check did not finish within 12 seconds",
        )
        .into()
    };
    config.validate()?;
    snapshots.send_modify(|snapshot| snapshot.error = None);
    let discovery = tokio::time::timeout_at(deadline, http.discover(&config.url))
        .await
        .map_err(|_| late())??;
    if let Some(left_out) = discovery
        .rejected
        .iter()
        .find(|entry| config.servers.contains(&entry.id))
    {
        return Err(format!(
            "the catalogue's server {:?} was left out: {}",
            left_out.id, left_out.error
        )
        .into());
    }
    let selected = selection::servers(&discovery.catalog, config)?;
    snapshots.send_modify(|snapshot| {
        snapshot.servers = discovery
            .catalog
            .servers
            .iter()
            .map(|entry| ServerSummary {
                id: entry.id.clone(),
                name: entry.name.clone(),
                origin: entry.url.clone(),
                ..ServerSummary::default()
            })
            .collect();
    });
    // Results publish as they arrive; catalogue order stays for lane planning and error selection.
    let mut checks = selected
        .iter()
        .enumerate()
        .map(|(index, entry)| async move {
            let check = tokio::time::timeout_at(deadline, prepare_server(config, http, entry));
            (index, check.await.map_err(|_| late()).and_then(|result| result))
        })
        .collect::<FuturesUnordered<_>>();
    let mut results: Vec<_> = (0..selected.len()).map(|_| None).collect();
    while let Some((index, result)) = checks.next().await {
        let entry = selected[index];
        snapshots.send_modify(|snapshot| {
            if let Some(summary) = snapshot.servers.iter_mut().find(|summary| summary.id == entry.id) {
                match &result {
                    Ok(server) => {
                        summary.throughput.clone_from(&server.throughput);
                        summary.latency.clone_from(&server.latency);
                    }
                    Err(error) => summary.error = Some(crate::failure::text(error.as_ref())),
                }
            }
        });
        results[index] = Some(result);
    }
    let mut prepared = Vec::with_capacity(selected.len());
    let mut failures = Vec::new();
    for (entry, result) in selected.iter().zip(results) {
        match result.expect("every selected verification completed") {
            Ok(server) => prepared.push(server),
            Err(source) => failures.push(ServerError {
                id: entry.id.clone(),
                label: entry.name.clone(),
                source,
            }),
        }
    }
    if prepared.is_empty() {
        return Err(preferred(failures));
    }
    Ok(Preparation {
        servers: prepared,
        failures,
    })
}

async fn prepare_server(config: &Config, http: &Http, entry: &ServerEntry) -> Result<PreparedServer, Error> {
    let transfers = config.stages.iter().any(|stage| stage.downloads() || stage.uploads());
    let needs_latency = config.loaded_latency || config.stages.contains(&Stage::Latency);
    let preflight = http.preflight(entry).await?;
    let client = http.for_server(entry, &preflight)?;
    let throughput_path = async {
        let mut throughput = transfers
            .then(|| selection::throughput(config, entry, &preflight, config.throughput_transport))
            .transpose()?;
        if config.stages.iter().any(|stage| stage.uploads()) && !preflight.capabilities.upload_checkpoint {
            return Err("selected server does not support authoritative upload checkpoints".into());
        }
        if let Some(target) = &throughput
            && target.transport != ThroughputTransport::FetchStream
        {
            match verify_throughput_webtransport(&client, target).await {
                Ok(()) => {}
                Err(error) if crate::failure::sign_in(error.as_ref()).is_some() => {
                    return Err(error);
                }
                Err(error) if config.throughput_transport.is_none() => {
                    throughput = Some(
                    selection::throughput(config, entry, &preflight, Some(ThroughputTransport::FetchStream))
                    .map_err(|fallback_error| -> Error {
                        format!(
                            "fetch-stream selection failed ({fallback_error}); advertised WebTransport is unavailable: {error}"
                        )
                        .into()
                    })?,
                );
                }
                Err(error) => return Err(error),
            }
        }
        let transport = if let Some(target) = &mut throughput {
            let connection = Transport::connect(client.clone(), &target.base_url, target.protocol).await?;
            if target.protocol == Protocol::Negotiated {
                (target.protocol, _) = client.probe(&target.base_url, Protocol::Negotiated).await?;
            } else {
                let probe: Probe = connection.json(Method::GET, Route::Probe, &[]).await?;
                probe.validate()?;
            }
            Some(Arc::new(connection))
        } else {
            None
        };
        Ok::<_, Error>((throughput, transport))
    };
    let latency_path = async {
        let Some(mut target) = needs_latency
            .then(|| selection::latency(config, entry, &preflight, config.latency_transport))
            .transpose()?
        else {
            return Ok((None, Duration::ZERO));
        };
        let rtt = match crate::latency::verify(&client, &target).await {
            Err(error)
                if target.transport == LatencyTransport::WebTransport
                    && config.latency_transport.is_none()
                    && crate::failure::sign_in(error.as_ref()).is_none() =>
            {
                target = selection::latency(config, entry, &preflight, Some(LatencyTransport::WebSocket))?;
                crate::latency::verify(&client, &target).await?
            }
            rtt => rtt?,
        };
        Ok::<_, Error>((Some(target), rtt))
    };
    let ((throughput, transport), (latency, idle_rtt)) = tokio::try_join!(throughput_path, latency_path)?;
    Ok(PreparedServer {
        entry: entry.clone(),
        client,
        throughput,
        http: transport,
        latency,
        idle_rtt,
        replaced_upload: Arc::default(),
    })
}

/// One dial within 3 s, as Go's verifyThroughputWebTransport (webtransport.go:74-83).
async fn verify_throughput_webtransport(http: &Http, target: &ThroughputTarget) -> Result<(), Error> {
    let origin = graphite_meter_core::origin::canonical_origin(&target.base_url)?;
    let target = crate::net::url(&origin, Route::WtDownload, &[("bytes", "0")]);
    let session = crate::webtransport::Session::dial(http, &target, Duration::from_secs(3)).await?;
    session.close().await;
    Ok(())
}

pub async fn run(
    config: Config,
    http: Http,
    snapshots: watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
    prepared: Option<PreparedRun>,
) -> Result<(), Error> {
    snapshots.send_modify(|snapshot| snapshot.start_run(&config.stages));
    let (mut prepared, lost) = match prepared.filter(|prepared| prepared.fresh_for(&config)) {
        Some(prepared) => (prepared.servers, 0),
        None => {
            let preparation = tokio::select! {
                result = prepare(&config, &http, &snapshots, Instant::now() + PREPARATION_TIMEOUT) => result?,
                _ = cancel.wait_for(|value| *value) => return Ok(()),
            };
            snapshots.send_modify(|snapshot| {
                snapshot.stage = config.stages.first().copied();
                // Go records these as its run starts, at zero.
                for failure in &preparation.failures {
                    let scope = crate::model::FailureScope::Throughput;
                    snapshot.failure(&failure.id, scope, &failure.source, Duration::ZERO);
                }
            });
            (preparation.servers, preparation.failures.len())
        }
    };
    // Go's coordinator starts its clock once the servers are prepared.
    let mut ledger = RunLedger::new();
    snapshots.send_modify(|snapshot| {
        snapshot.participants = prepared.iter().map(|server| server.entry.id.clone()).collect();
        snapshot.latency_focus = prepared.first().map(|server| server.entry.id.clone());
    });
    let sole = (prepared.len() + lost == 1).then(|| prepared[0].entry.id.clone());
    for stage in &config.stages {
        if *cancel.borrow() {
            break;
        }
        // As Go's openStage: a sole server whose stage was skipped rejoins on the transport it prepared.
        if let Some(sole) = &sole {
            snapshots.send_if_modified(|snapshot| {
                let rejoins = !snapshot.participants.contains(sole);
                if rejoins {
                    snapshot.participants.push(sole.clone());
                }
                rejoins
            });
        }
        let measured = measure(*stage, &config, &prepared, &snapshots, cancel.clone(), &mut ledger).await;
        let failed = match measured {
            Ok(failed) => failed,
            Err(error)
                if sole.is_some()
                    && crate::failure::sign_in(error.as_ref()).is_none()
                    && snapshots.borrow().measured() =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        // An idle latency stage's medians pace the later stages' warmups and lane staggers.
        if let Some(result) = snapshots.borrow().results.last().filter(|_| *stage == Stage::Latency) {
            for measured in &result.server_latencies {
                if let Some(distribution) = measured.summary.distribution
                    && let Some(server) = prepared.iter_mut().find(|server| server.entry.id == measured.id)
                {
                    server.idle_rtt = Duration::from_nanos(distribution.p50);
                }
            }
        }
        prepared.retain(|server| !failed.contains(&server.entry.id));
    }
    let stopped = *cancel.borrow();
    snapshots.send_modify(|snapshot| snapshot.finish_run(stopped));
    Ok(())
}

mod stage;
use stage::{RunLedger, measure};
