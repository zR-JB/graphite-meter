//! Run preparation and measurement are independent of terminal rendering.
use crate::{
    Error,
    config::Config,
    model::{Phase, Point, ServerSummary, Snapshot, Stage},
    net::Http,
    selection,
    transport::Transport,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::route::Route;
use graphite_meter_core::{
    catalog::ServerEntry,
    discovery::{
        LatencyTarget, LatencyTransport, Probe, Protocol, ProtocolNegotiated, ThroughputTarget, ThroughputTransport,
    },
};
use http::Method;
use std::{sync::Arc, time::Duration};
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
struct PreparationFailure {
    id: String,
    name: String,
    source: Error,
}

struct Preparation {
    servers: Vec<PreparedServer>,
    failures: Vec<PreparationFailure>,
}

/// The controller approves one origin per retry, so prefer a sign-in challenge.
fn preferred(mut failures: Vec<PreparationFailure>) -> Error {
    let index = failures
        .iter()
        .position(|failure| crate::net::authentication_required(failure.source.as_ref()).is_some())
        .unwrap_or(0);
    failures.swap_remove(index).into()
}

impl std::fmt::Display for PreparationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.name, self.source)
    }
}

impl std::error::Error for PreparationFailure {
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
    snapshots.send_modify(|snapshot| {
        snapshot.error = None;
        snapshot.status = "Loading server catalogue".into();
    });
    let discovery = tokio::time::timeout_at(deadline, http.discover(&config.url))
        .await
        .map_err(|_| late())??;
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
    let transfers = config.stages.iter().any(|stage| stage.downloads() || stage.uploads());
    let latency = config.loaded_latency || config.stages.contains(&crate::model::Stage::Latency);
    snapshots.send_modify(|snapshot| {
        snapshot.status = format!("Verifying {} selected servers", selected.len());
    });
    // Results publish as they arrive; catalogue order stays for lane planning and error selection.
    let mut checks = selected
        .iter()
        .enumerate()
        .map(|(index, entry)| async move {
            let check = tokio::time::timeout_at(deadline, prepare_server(config, http, entry, transfers, latency));
            (index, check.await.map_err(|_| late()).and_then(|result| result))
        })
        .collect::<FuturesUnordered<_>>();
    let mut results: Vec<_> = (0..selected.len()).map(|_| None).collect();
    let mut completed = 0;
    while let Some((index, result)) = checks.next().await {
        completed += 1;
        let entry = selected[index];
        snapshots.send_modify(|snapshot| {
            if let Some(summary) = snapshot.servers.iter_mut().find(|summary| summary.id == entry.id) {
                match &result {
                    Ok(server) => {
                        summary.throughput.clone_from(&server.throughput);
                        summary.latency.clone_from(&server.latency);
                    }
                    Err(error) => summary.error = Some(error.to_string()),
                }
            }
            snapshot.status = format!("Verified {completed}/{} selected servers", selected.len());
        });
        results[index] = Some(result);
    }
    let mut prepared = Vec::with_capacity(selected.len());
    let mut failures = Vec::new();
    for (entry, result) in selected.iter().zip(results) {
        match result.expect("every selected verification completed") {
            Ok(server) => prepared.push(server),
            Err(source) => failures.push(PreparationFailure {
                id: entry.id.clone(),
                name: entry.name.clone(),
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

async fn prepare_server(
    config: &Config,
    http: &Http,
    entry: &ServerEntry,
    transfers: bool,
    needs_latency: bool,
) -> Result<PreparedServer, Error> {
    let preflight = http.preflight(entry).await?;
    let client = http.for_server(entry, &preflight)?;
    let throughput_path = async {
        let mut throughput = transfers
            .then(|| selection::throughput(config, entry, &preflight))
            .transpose()?;
        if config.stages.iter().any(|stage| stage.uploads()) && !preflight.capabilities.upload_checkpoint {
            return Err("selected server does not support authoritative upload checkpoints".into());
        }
        if let Some(target) = &throughput
            && target.transport != ThroughputTransport::FetchStream
        {
            match verify_throughput_webtransport(&client, target, config.insecure).await {
                Ok(()) => {}
                Err(error) if crate::net::authentication_required(error.as_ref()).is_some() => {
                    return Err(error);
                }
                Err(error) if config.throughput_transport.is_none() => {
                    throughput = Some(
                    selection::throughput_with_transport(
                        config,
                        entry,
                        &preflight,
                        ThroughputTransport::FetchStream,
                    )
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
            let connection =
                Transport::connect(client.clone(), &target.base_url, target.protocol, config.insecure).await?;
            if target.protocol == Protocol::Negotiated {
                let probe = client.probe(&target.base_url, Protocol::Negotiated).await?;
                target.protocol = match probe.protocol_negotiated {
                    ProtocolNegotiated::Http1 => Protocol::Http1,
                    ProtocolNegotiated::Http2 => Protocol::Http2,
                    ProtocolNegotiated::Http3 => {
                        return Err("negotiated HTTP probe cannot use HTTP/3".into());
                    }
                };
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
            .then(|| selection::latency(config, entry, &preflight))
            .transpose()?
        else {
            return Ok((None, Duration::ZERO));
        };
        let rtt = match crate::latency::verify(&client, &target, config.insecure).await {
            Err(error)
                if target.transport == LatencyTransport::WebTransport
                    && config.latency_transport.is_none()
                    && crate::net::authentication_required(error.as_ref()).is_none() =>
            {
                target = selection::latency_with_transport(config, entry, &preflight, LatencyTransport::WebSocket)?;
                crate::latency::verify(&client, &target, config.insecure).await?
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
    })
}

async fn verify_throughput_webtransport(http: &Http, target: &ThroughputTarget, insecure: bool) -> Result<(), Error> {
    let origin = graphite_meter_core::origin::canonical_origin(&target.base_url)?;
    let url = format!("{origin}{}?bytes=0", Route::WtDownload.path());
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut backoff = crate::transport::RetryBackoff::default();
    loop {
        let started = Instant::now();
        let error = match tokio::time::timeout_at(
            deadline,
            crate::webtransport::Session::dial(http, &url, insecure, Duration::from_secs(3)),
        )
        .await?
        {
            Ok(session) => {
                session.close().await;
                return Ok(());
            }
            Err(error) => error,
        };
        if crate::failure::reason(error.as_ref(), false) != graphite_meter_core::failure::FailureReason::ServerBusy
            || Instant::now() >= deadline
        {
            return Err(error);
        }
        tokio::time::sleep_until((Instant::now() + backoff.delay(error.as_ref(), started)).min(deadline)).await;
        if Instant::now() >= deadline {
            return Err(error);
        }
    }
}

pub async fn run(
    config: Config,
    http: Http,
    snapshots: watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
    prepared: Option<PreparedRun>,
) -> Result<(), Error> {
    snapshots.send_modify(|snapshot| {
        snapshot.plan.clone_from(&config.stages);
        snapshot.results.clear();
        snapshot.failures.clear();
        snapshot.server_latencies.clear();
        snapshot.participants.clear();
        snapshot.latency_focus = None;
        snapshot.history = Default::default();
        snapshot.latest = Point::default();
        snapshot.stage = None;
    });
    let (mut prepared, selected) = match prepared.filter(|prepared| prepared.fresh_for(&config)) {
        Some(prepared) => (prepared.servers, None),
        None => {
            let preparation = tokio::select! {
                result = prepare(&config, &http, &snapshots, Instant::now() + PREPARATION_TIMEOUT) => result?,
                _ = cancel.wait_for(|value| *value) => return Ok(()),
            };
            snapshots.send_modify(|snapshot| {
                snapshot.stage = config.stages.first().copied();
                for failure in &preparation.failures {
                    snapshot.failure(&failure.id, crate::model::FailureScope::Throughput, &failure.source);
                }
            });
            let selected = preparation.servers.len() + preparation.failures.len();
            (preparation.servers, Some(selected))
        }
    };
    snapshots.send_modify(|snapshot| {
        snapshot.participants = prepared.iter().map(|server| server.entry.id.clone()).collect();
        snapshot.latency_focus = prepared.first().map(|server| server.entry.id.clone());
    });
    let sole = (selected.unwrap_or(prepared.len()) == 1).then(|| prepared[0].entry.clone());
    let mut retry_sole = false;
    for stage in &config.stages {
        if *cancel.borrow() {
            break;
        }
        if retry_sole {
            snapshots.send_modify(|snapshot| {
                snapshot.phase = Phase::Preparing;
                snapshot.stage = Some(*stage);
                snapshot.latest = Point::default();
                snapshot.status = format!("Preparing {}", stage.name());
            });
            let entry = sole.as_ref().expect("sole-server retry");
            let transfer = stage.downloads() || stage.uploads();
            let latency = *stage == Stage::Latency || config.loaded_latency;
            let replacement = tokio::select! {
                result = tokio::time::timeout(PREPARATION_TIMEOUT, prepare_server(&config, &http, entry, transfer, latency)) => result.map_err(Error::from).and_then(|result| result),
                _ = cancel.wait_for(|value| *value) => break,
            };
            match replacement {
                Ok(server) => {
                    snapshots.send_modify(|snapshot| {
                        if let Some(summary) = snapshot.servers.iter_mut().find(|summary| summary.id == entry.id) {
                            summary.throughput.clone_from(&server.throughput);
                            summary.latency.clone_from(&server.latency);
                        }
                    });
                    prepared = vec![server];
                    retry_sole = false;
                }
                Err(error) => {
                    snapshots.send_modify(|snapshot| {
                        snapshot.stage = Some(*stage);
                        snapshot.failure(
                            &entry.id,
                            if transfer {
                                crate::model::FailureScope::Throughput
                            } else {
                                crate::model::FailureScope::Latency
                            },
                            &error,
                        );
                        let ending = crate::model::Ending::Failed(crate::failure::reason(error.as_ref(), true));
                        snapshot.results.push(crate::model::StageResult {
                            stage: *stage,
                            elapsed: Duration::ZERO,
                            down: None,
                            up: None,
                            intervals: Default::default(),
                            omitted_intervals: 0,
                            stopped: false,
                            server_results: vec![crate::model::ServerContribution {
                                id: entry.id.clone(),
                                down: None,
                                up: None,
                            }],
                            server_latencies: vec![crate::model::ServerLatencyResult {
                                elapsed: None,
                                id: entry.id.clone(),
                                summary: Default::default(),
                                ending: Some(ending),
                            }],
                        });
                    });
                    continue;
                }
            }
        }
        let measured = measure(*stage, &config, &prepared, &snapshots, cancel.clone()).await;
        let failed = match measured {
            Ok(failed) => failed,
            Err(error)
                if sole.is_some()
                    && crate::net::authentication_required(error.as_ref()).is_none()
                    && snapshots
                        .borrow()
                        .results
                        .iter()
                        .any(|result| result.elapsed > Duration::ZERO) =>
            {
                retry_sole = true;
                continue;
            }
            Err(error) => return Err(error),
        };
        if *stage == Stage::Latency {
            let snapshot = snapshots.borrow();
            if let Some(result) = snapshot.results.last() {
                for measured in &result.server_latencies {
                    if let Some(distribution) = measured.summary.distribution
                        && let Some(server) = prepared.iter_mut().find(|server| server.entry.id == measured.id)
                    {
                        server.idle_rtt = Duration::from_nanos(distribution.p50);
                    }
                }
            }
        }
        prepared.retain(|server| !failed.contains(&server.entry.id));
    }
    let stopped = *cancel.borrow();
    snapshots.send_modify(|snapshot| {
        let missing = snapshot
            .results
            .iter()
            .any(|result| snapshot.stage_status(result) == crate::model::StageStatus::Failed);
        (snapshot.phase, snapshot.status) = if stopped {
            (Phase::Cancelled, "Stopped".into())
        } else if missing {
            (Phase::Incomplete, "Incomplete".into())
        } else if !snapshot.failures.is_empty() {
            (Phase::Partial, "Partial".into())
        } else {
            (Phase::Complete, "Complete".into())
        };
    });
    Ok(())
}

mod stage;
use stage::measure;
