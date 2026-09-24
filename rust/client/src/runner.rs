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
use graphite_meter_core::route::Route;
use graphite_meter_core::{
    catalog::ServerEntry,
    discovery::{LatencyTarget, LatencyTransport, Probe, ThroughputTarget, ThroughputTransport},
};
use http::Method;
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};

#[cfg(test)]
mod prepare_tests;

struct PreparedServer {
    entry: ServerEntry,
    throughput: Option<ThroughputTarget>,
    http: Option<Arc<Transport>>,
    latency: Option<LatencyTarget>,
    idle_rtt: Duration,
}

pub async fn verify(
    config: &Config,
    http: &Http,
    snapshots: &watch::Sender<Snapshot>,
) -> Result<(), Error> {
    prepare(config, http, snapshots).await?;
    snapshots.send_modify(|snapshot| {
        snapshot.phase = Phase::Setup;
        snapshot.status = "Selected servers verified".into();
    });
    Ok(())
}

async fn prepare(
    config: &Config,
    http: &Http,
    snapshots: &watch::Sender<Snapshot>,
) -> Result<Vec<PreparedServer>, Error> {
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
    let mut prepared = Vec::with_capacity(selected.len());
    for entry in selected {
        snapshots.send_modify(|snapshot| snapshot.status = format!("Verifying {}", entry.name));
        let preflight = http.preflight(entry).await?;
        http.approve_targets(entry, &preflight)?;
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
            match verify_throughput_webtransport(http, target, config.insecure).await {
                Ok(()) => {}
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
        let mut idle_rtt = Duration::ZERO;
        let transport = if let Some(target) = &throughput {
            let connection = Transport::connect(
                http.clone(),
                &target.base_url,
                target.protocol,
                config.insecure,
            )
            .await?;
            let probe: Probe = connection.json(Method::GET, Route::Probe, &[]).await?;
            probe.validate()?;
            Some(Arc::new(connection))
        } else {
            None
        };
        let mut latency = latency
            .then(|| selection::latency(config, entry, &preflight))
            .transpose()?;
        if let Some(target) = &latency
            && target.transport == LatencyTransport::WebTransport
        {
            match crate::latency::verify(http, target, config.insecure).await {
                Ok(()) => {}
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
            http.probe(
                &target.base_url,
                graphite_meter_core::discovery::Protocol::Negotiated,
            )
            .await?;
            idle_rtt = started.elapsed();
            if target.transport == LatencyTransport::WebSocket {
                crate::latency::verify(http, target, config.insecure).await?;
            }
        }
        snapshots.send_modify(|snapshot| {
            if let Some(summary) = snapshot
                .servers
                .iter_mut()
                .find(|summary| summary.id == entry.id)
            {
                summary.transport = throughput.as_ref().map_or_else(
                    || "Latency".into(),
                    |target| format!("{:?} / {:?}", target.transport, target.protocol),
                );
            }
        });
        prepared.push(PreparedServer {
            entry: entry.clone(),
            throughput,
            http: transport,
            latency,
            idle_rtt,
        });
    }
    for stage in &config.stages {
        lane_plan(config, *stage, &prepared)?;
    }
    Ok(prepared)
}

async fn verify_throughput_webtransport(
    http: &Http,
    target: &ThroughputTarget,
    insecure: bool,
) -> Result<(), Error> {
    let origin = graphite_meter_core::origin::canonical_origin(&target.base_url)?;
    let query = match target.transport {
        ThroughputTransport::WebTransport => "bytes=0",
        ThroughputTransport::WebTransportDatagram => "bytes=0&datagrams=1",
        ThroughputTransport::FetchStream => return Err("fetch stream is not WebTransport".into()),
    };
    let url = format!("{origin}{}?{query}", Route::WtDownload.path());
    crate::webtransport::Session::dial(http, &url, insecure, Duration::from_secs(3))
        .await?
        .close()
        .await;
    Ok(())
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
    mut cancel: watch::Receiver<bool>,
) -> Result<(), Error> {
    snapshots.send_modify(|snapshot| {
        snapshot.results.clear();
        snapshot.server_latencies.clear();
        snapshot.history.clear();
        snapshot.latest = Point::default();
        snapshot.stage = None;
    });
    let mut prepared = tokio::select! {
        result = prepare(&config, &http, &snapshots) => result?,
        _ = cancel.wait_for(|value| *value) => return Ok(()),
    };
    for stage in &config.stages {
        if *cancel.borrow() {
            break;
        }
        let completed_stage = snapshots
            .borrow()
            .results
            .iter()
            .any(|result| result.complete);
        let failed = measure(
            *stage,
            &config,
            &http,
            &prepared,
            &snapshots,
            cancel.clone(),
            completed_stage,
        )
        .await?;
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
        prepared.retain(|server| !failed.contains(&server.entry.id));
    }
    snapshots.send_modify(|snapshot| {
        snapshot.phase = if *cancel.borrow() {
            Phase::Cancelled
        } else {
            Phase::Complete
        };
        snapshot.status = if *cancel.borrow() {
            "Cancelled"
        } else if snapshot.servers.iter().any(|server| server.error.is_some())
            || snapshot.results.iter().any(|result| !result.complete)
        {
            "Finished with partial results"
        } else {
            "Measurement complete"
        }
        .into();
    });
    Ok(())
}

mod stage;
use stage::measure;
