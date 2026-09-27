//! Run preparation and measurement are independent of terminal rendering.
use crate::{
    Error,
    config::Config,
    model::{Phase, Point, ServerSummary, Snapshot, Stage},
    net::Http,
    selection,
    stream_plan::{Participant, StageLanePlan},
    transport::Transport,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::route::Route;
use graphite_meter_core::{
    catalog::ServerEntry,
    discovery::{
        LatencyTarget, LatencyTransport, Probe, Protocol, ProtocolNegotiated, ThroughputTarget,
        ThroughputTransport,
    },
};
use http::Method;
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};

#[cfg(test)]
mod prepare_tests;

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
        self.key == config.preparation_key()
            && self.verified_at.elapsed() <= Duration::from_secs(30)
    }
}

pub async fn prepare_run(
    config: &Config,
    http: &Http,
    snapshots: &watch::Sender<Snapshot>,
) -> Result<PreparedRun, Error> {
    let verified_at = Instant::now();
    let preparation = prepare(config, http, snapshots).await?;
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

async fn prepare(
    config: &Config,
    http: &Http,
    snapshots: &watch::Sender<Snapshot>,
) -> Result<Preparation, Error> {
    tokio::time::timeout(
        Duration::from_secs(12),
        prepare_inner(config, http, snapshots),
    )
    .await
    .map_err(|_| -> Error {
        Box::new(crate::failure::MeasurementFailure(
            graphite_meter_core::failure::FailureReason::Timeout,
        ))
    })?
}

async fn prepare_inner(
    config: &Config,
    http: &Http,
    snapshots: &watch::Sender<Snapshot>,
) -> Result<Preparation, Error> {
    config.validate()?;
    snapshots.send_modify(|snapshot| {
        snapshot.phase = Phase::Preparing;
        snapshot.error = None;
        snapshot.status = "Loading server catalogue".into();
    });
    let discovery = http.discover(&config.url).await?;
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
    let transfers = config
        .stages
        .iter()
        .any(|stage| stage.downloads() || stage.uploads());
    let latency = config.loaded_latency || config.stages.contains(&crate::model::Stage::Latency);
    snapshots.send_modify(|snapshot| {
        snapshot.status = format!("Verifying {} selected servers", selected.len());
    });
    // Run independent origins concurrently and publish each result as it
    // arrives. Retain catalogue order for lane planning and error selection.
    let mut checks = selected
        .iter()
        .enumerate()
        .map(|(index, entry)| async move {
            (
                index,
                prepare_server(config, http, entry, transfers, latency).await,
            )
        })
        .collect::<FuturesUnordered<_>>();
    let mut results: Vec<_> = (0..selected.len()).map(|_| None).collect();
    let mut completed = 0;
    while let Some((index, result)) = checks.next().await {
        completed += 1;
        let entry = selected[index];
        snapshots.send_modify(|snapshot| {
            if let Some(summary) = snapshot
                .servers
                .iter_mut()
                .find(|summary| summary.id == entry.id)
            {
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
    for stage in &config.stages {
        lane_plan(config, *stage, &prepared)?;
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
        if config.stages.iter().any(|stage| stage.uploads())
            && !preflight.capabilities.upload_checkpoint
        {
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
            let connection = Transport::connect(
                client.clone(),
                &target.base_url,
                target.protocol,
                config.insecure,
            )
            .await?;
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
        let mut idle_rtt = Duration::ZERO;
        let mut latency = needs_latency
            .then(|| selection::latency(config, entry, &preflight))
            .transpose()?;
        if let Some(target) = &latency
            && target.transport == LatencyTransport::WebTransport
        {
            match crate::latency::verify(&client, target, config.insecure).await {
                Ok(()) => {}
                Err(error) if crate::net::authentication_required(error.as_ref()).is_some() => {
                    return Err(error);
                }
                Err(error) if config.latency_transport.is_none() => {
                    latency = Some(
                    selection::latency_with_transport(
                        config,
                        entry,
                        &preflight,
                        LatencyTransport::WebSocket,
                    )
                    .map_err(|fallback_error| -> Error {
                        format!(
                            "WebTransport latency unavailable ({error}); WebSocket fallback: {fallback_error}"
                        )
                        .into()
                    })?,
                );
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(target) = &latency {
            if target.transport == LatencyTransport::WebTransport
                && config.ping_interval > Duration::from_secs(15)
            {
                return Err("WebTransport ping interval must not exceed 15 seconds".into());
            }
            let started = Instant::now();
            client
                .probe(
                    &target.base_url,
                    graphite_meter_core::discovery::Protocol::Negotiated,
                )
                .await?;
            idle_rtt = started.elapsed();
            if target.transport == LatencyTransport::WebSocket {
                crate::latency::verify(&client, target, config.insecure).await?;
            }
        }
        Ok::<_, Error>((latency, idle_rtt))
    };
    let ((throughput, transport), (latency, idle_rtt)) =
        tokio::try_join!(throughput_path, latency_path)?;
    Ok(PreparedServer {
        entry: entry.clone(),
        client,
        throughput,
        http: transport,
        latency,
        idle_rtt,
    })
}

async fn verify_throughput_webtransport(
    http: &Http,
    target: &ThroughputTarget,
    insecure: bool,
) -> Result<(), Error> {
    let origin = graphite_meter_core::origin::canonical_origin(&target.base_url)?;
    let url = format!("{origin}{}?bytes=0", Route::WtDownload.path());
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut backoff = crate::transport::RetryBackoff::default();
    loop {
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
        if crate::failure::reason(error.as_ref(), false)
            != graphite_meter_core::failure::FailureReason::ServerBusy
            || Instant::now() >= deadline
        {
            return Err(error);
        }
        tokio::time::sleep_until((Instant::now() + backoff.delay(error.as_ref())).min(deadline))
            .await;
        if Instant::now() >= deadline {
            return Err(error);
        }
    }
}

fn lane_plan(
    config: &Config,
    stage: Stage,
    servers: &[PreparedServer],
) -> Result<StageLanePlan, Error> {
    let participants: Vec<_> = servers
        .iter()
        .map(|server| Participant {
            id: &server.entry.id,
            throughput: server.throughput.as_ref(),
            latency: server.latency.as_ref(),
        })
        .collect();
    StageLanePlan::new(config, stage, &participants)
}

pub async fn run(
    config: Config,
    http: Http,
    snapshots: watch::Sender<Snapshot>,
    cancel: watch::Receiver<bool>,
) -> Result<(), Error> {
    run_prepared(config, http, snapshots, cancel, None).await
}

pub async fn run_prepared(
    config: Config,
    http: Http,
    snapshots: watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
    prepared: Option<PreparedRun>,
) -> Result<(), Error> {
    snapshots.send_modify(|snapshot| {
        snapshot.results.clear();
        snapshot.failures.clear();
        snapshot.server_latencies.clear();
        snapshot.history = Default::default();
        snapshot.latest = Point::default();
        snapshot.stage = None;
    });
    let mut prepared = match prepared.filter(|prepared| prepared.fresh_for(&config)) {
        Some(prepared) => prepared.servers,
        None => {
            let preparation = tokio::select! {
                result = prepare(&config, &http, &snapshots) => result?,
                _ = cancel.wait_for(|value| *value) => return Ok(()),
            };
            snapshots.send_modify(|snapshot| {
                snapshot.stage = config.stages.first().copied();
                for failure in &preparation.failures {
                    snapshot.failure(
                        &failure.id,
                        crate::model::FailureScope::Throughput,
                        &failure.source,
                    );
                }
            });
            preparation.servers
        }
    };
    let sole = (prepared.len() == 1).then(|| prepared[0].entry.clone());
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
                result = tokio::time::timeout(Duration::from_secs(12), prepare_server(&config, &http, entry, transfer, latency)) => result.map_err(Error::from).and_then(|result| result),
                _ = cancel.wait_for(|value| *value) => break,
            };
            match replacement {
                Ok(server) => {
                    snapshots.send_modify(|snapshot| {
                        if let Some(summary) = snapshot
                            .servers
                            .iter_mut()
                            .find(|summary| summary.id == entry.id)
                        {
                            summary.error = None;
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
                        if let Some(summary) = snapshot
                            .servers
                            .iter_mut()
                            .find(|summary| summary.id == entry.id)
                        {
                            summary.error = Some(error.to_string());
                        }
                        snapshot.results.push(crate::model::StageResult {
                            stage: *stage,
                            elapsed: Duration::ZERO,
                            down: None,
                            up: None,
                            intervals: Default::default(),
                            omitted_intervals: 0,
                            complete: false,
                            server_results: vec![crate::model::ServerContribution {
                                id: entry.id.clone(),
                                down: None,
                                up: None,
                                error: Some(error.to_string()),
                            }],
                            server_latencies: vec![crate::model::ServerLatencyResult {
                                elapsed: None,
                                id: entry.id.clone(),
                                summary: Default::default(),
                                error: Some(error.to_string()),
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
            Err(_)
                if sole.is_some()
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
                        && let Some(server) = prepared
                            .iter_mut()
                            .find(|server| server.entry.id == measured.id)
                    {
                        server.idle_rtt = Duration::from_nanos(distribution.p50);
                    }
                }
            }
        }
        if sole.is_some() && !failed.is_empty() {
            retry_sole = true;
        } else {
            prepared.retain(|server| !failed.contains(&server.entry.id));
        }
    }
    snapshots.send_modify(|snapshot| {
        let partial = snapshot.servers.iter().any(|server| server.error.is_some())
            || snapshot.results.iter().any(|result| !result.complete);
        (snapshot.phase, snapshot.status) = if *cancel.borrow() {
            (Phase::Cancelled, "Stopped".into())
        } else if snapshot
            .results
            .iter()
            .any(|result| result.status() == crate::model::StageStatus::Failed)
        {
            (Phase::Incomplete, "Incomplete".into())
        } else if partial {
            (Phase::Partial, "Partial".into())
        } else {
            (Phase::Complete, "Complete".into())
        };
    });
    Ok(())
}

mod stage;
use stage::measure;
