//! Run preparation and measurement are independent of terminal rendering.
use crate::{
    Error,
    config::Config,
    model::{Phase, ServerSummary, Snapshot},
    net::Http,
    selection,
    transport::Transport,
};
use graphite_meter_core::route::Route;
use graphite_meter_core::{
    catalog::ServerEntry,
    discovery::{LatencyTarget, Probe, ThroughputTarget},
};
use http::Method;
use std::sync::Arc;
use tokio::sync::watch;

struct PreparedServer {
    entry: ServerEntry,
    throughput: Option<ThroughputTarget>,
    http: Option<Arc<Transport>>,
    latency: Option<LatencyTarget>,
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
    for (index, entry) in selected.into_iter().enumerate() {
        snapshots.send_modify(|snapshot| snapshot.status = format!("Verifying {}", entry.name));
        let preflight = http.preflight(entry).await?;
        http.approve_targets(entry, &preflight)?;
        let throughput = transfers
            .then(|| selection::throughput(config, entry, &preflight))
            .transpose()?;
        if config.stages.iter().any(|stage| stage.uploads())
            && !preflight.capabilities.upload_checkpoint
        {
            return Err("selected server does not support authoritative upload checkpoints".into());
        }
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
        let latency = if !latency {
            None
        } else if index == 0 {
            Some(selection::latency(config, entry, &preflight)?)
        } else {
            // Optional fallback for a later stage if the primary drops out.
            // A throughput-only secondary never blocks the selected run.
            selection::latency(config, entry, &preflight).ok()
        };
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
        });
    }
    Ok(prepared)
}

use crate::{
    download::Download,
    latency::Observation,
    model::{Point, Stage, StageResult},
    upload::Upload,
};
use graphite_meter_core::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    latency::LatencyAccumulator,
    measurement::{
        AggregateMeasurements, Boundary, Direction, IntervalReason, Stage as TransferStage,
    },
};
use std::time::Duration;
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};

pub async fn run(
    config: Config,
    http: Http,
    snapshots: watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), Error> {
    snapshots.send_modify(|snapshot| {
        snapshot.results.clear();
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
        let failed = measure(
            *stage,
            &config,
            &http,
            &prepared,
            &snapshots,
            cancel.clone(),
        )
        .await?;
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

struct Transfer {
    id: String,
    down: Option<Download>,
    up: Option<Upload>,
}

struct StageResources {
    transfers: Vec<Transfer>,
    latency: JoinSet<Result<(), Error>>,
    stop: watch::Sender<bool>,
    stop_latency: watch::Sender<bool>,
    retired: JoinSet<Result<(), Error>>,
    failed: Vec<String>,
    latency_failed: bool,
}

#[derive(Debug)]
struct ParticipantFailure {
    id: String,
    source: Error,
}
impl std::fmt::Display for ParticipantFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.id, self.source)
    }
}
impl std::error::Error for ParticipantFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
struct LatencyFailure(Error);
impl std::fmt::Display for LatencyFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "latency unavailable: {}", self.0)
    }
}
impl std::error::Error for LatencyFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

impl Transfer {
    async fn close(self) -> Result<(), Error> {
        if let Some(down) = self.down {
            down.stop().await;
        }
        if let Some(up) = self.up {
            up.finish().await?;
        }
        Ok(())
    }
}

impl StageResources {
    async fn close(mut self) -> Result<(), Error> {
        self.stop.send_replace(true);
        self.stop_latency.send_replace(true);
        let mut failure = None;
        let finalizers = self.transfers.drain(..).map(Transfer::close);
        for result in futures_util::future::join_all(finalizers).await {
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        while let Some(result) = self.latency.join_next().await {
            if let Err(error) = result.map_err(Error::from).and_then(|result| result) {
                failure.get_or_insert(error);
            }
        }
        // Retired participants already have a recorded failure. Reap their
        // bounded cleanup without turning a survivor result into another failure.
        while self.retired.join_next().await.is_some() {}
        failure.map_or(Ok(()), Err)
    }

    fn health(&mut self) -> Result<(), Error> {
        for transfer in &mut self.transfers {
            if let Some(down) = &mut transfer.down {
                down.health().map_err(|source| ParticipantFailure {
                    id: transfer.id.clone(),
                    source,
                })?;
            }
            if let Some(up) = &mut transfer.up {
                up.health().map_err(|source| ParticipantFailure {
                    id: transfer.id.clone(),
                    source,
                })?;
            }
        }
        if let Some(result) = self.latency.try_join_next() {
            let source = result
                .map_err(Error::from)
                .and_then(|result| result)
                .err()
                .unwrap_or_else(|| "latency session ended before stage boundary".into());
            return Err(LatencyFailure(source).into());
        }
        Ok(())
    }

    fn recover(
        &mut self,
        error: Error,
        accounting: &mut AggregateMeasurements,
        stage: Option<TransferStage>,
        epoch: Instant,
        snapshots: &watch::Sender<Snapshot>,
    ) -> Result<(), Error> {
        if error.is::<LatencyFailure>() && stage.is_some() {
            self.latency_failed = true;
            self.stop_latency.send_replace(true);
            snapshots.send_modify(|snapshot| {
                snapshot.status = format!("{error}; throughput measurement continues");
            });
            return Ok(());
        }
        let failure = error.downcast::<ParticipantFailure>()?;
        let Some(index) = self
            .transfers
            .iter()
            .position(|transfer| transfer.id == failure.id)
        else {
            return Err(failure);
        };
        accounting.observe(self.local_boundary(epoch));
        let transfer = self.transfers.remove(index);
        snapshots.send_modify(|snapshot| {
            if let Some(server) = snapshot
                .servers
                .iter_mut()
                .find(|server| server.id == failure.id)
            {
                server.error = Some(failure.source.to_string());
            }
            snapshot.status = format!(
                "{} unavailable; continuing with remaining servers",
                failure.id
            );
        });
        self.failed.push(failure.id);
        self.retired.spawn(transfer.close());
        if self.transfers.is_empty() {
            return Err("all selected servers failed".into());
        }
        if let Some(stage) = stage {
            accounting.begin(
                stage,
                self.transfers
                    .iter()
                    .map(|transfer| transfer.id.clone())
                    .collect(),
                nanos(epoch.elapsed()),
                IntervalReason::Dropout,
            );
        }
        Ok(())
    }

    fn local_boundary(&self, epoch: Instant) -> Boundary {
        let mut boundary = Boundary {
            at_nanos: nanos(epoch.elapsed()),
            ..Boundary::default()
        };
        for transfer in &self.transfers {
            if let Some(down) = &transfer.down {
                boundary.down.insert(transfer.id.clone(), down.bytes());
            }
            if let Some(up) = &transfer.up
                && let Some(observed) = up.observed()
            {
                boundary.observed_up.insert(transfer.id.clone(), observed);
            }
        }
        boundary
    }

    async fn boundary(&self, epoch: Instant) -> Result<Boundary, Error> {
        // Snapshot every local counter before waiting on any remote clock.
        // Parallel checkpoints prevent one server's RTT from shifting its peers.
        let mut boundary = self.local_boundary(epoch);
        let checkpoints = self.transfers.iter().filter_map(|transfer| {
            transfer.up.as_ref().map(|up| async move {
                Ok::<_, Error>((
                    transfer.id.clone(),
                    up.checkpoint().await.map_err(|source| ParticipantFailure {
                        id: transfer.id.clone(),
                        source,
                    })?,
                ))
            })
        });
        boundary
            .up
            .extend(futures_util::future::try_join_all(checkpoints).await?);
        Ok(boundary)
    }
}

async fn measure(
    stage: Stage,
    config: &Config,
    http: &Http,
    servers: &[PreparedServer],
    snapshots: &watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
) -> Result<Vec<String>, Error> {
    let epoch = Instant::now();
    let (stop, stopped) = watch::channel(false);
    let (stop_latency, latency_stopped) = watch::channel(false);
    let mut resources = StageResources {
        transfers: Vec::new(),
        latency: JoinSet::new(),
        stop,
        stop_latency,
        retired: JoinSet::new(),
        failed: Vec::new(),
        latency_failed: false,
    };
    let (observations, mut events) = mpsc::channel(1024);
    let operation_limit = config
        .warmup
        .checked_add(config.duration(stage))
        .and_then(|duration| duration.checked_add(Duration::from_secs(60)))
        .ok_or("stage duration overflow")?;
    snapshots.send_modify(|snapshot| {
        snapshot.phase = Phase::Preparing;
        snapshot.stage = Some(stage);
        snapshot.status = format!("Preparing {}", stage.name());
        snapshot.history.clear();
        snapshot.latest = Point::default();
    });
    let transfer_stage = match stage {
        Stage::Latency => None,
        Stage::Download => Some(TransferStage::Download),
        Stage::Upload => Some(TransferStage::Upload),
        Stage::Bidirectional => Some(TransferStage::Bidirectional),
    };
    let mut accounting = AggregateMeasurements::default();
    let mut latency = LatencyAccumulator::default();
    let mut latest_latency = None;
    let mut measurement_start = None;
    let mut measurement_end = None;
    let operation = async {
        for server in servers {
            let mut transfer = Transfer {
                id: server.entry.id.clone(),
                down: None,
                up: None,
            };
            if stage.downloads() || stage.uploads() {
                let target = server
                    .throughput
                    .as_ref()
                    .ok_or("missing throughput target")?;
                let transport = server
                    .http
                    .as_ref()
                    .ok_or("missing throughput connection")?;
                let lanes = |upload| stream_count(config, target, servers.len(), upload);
                if stage.downloads() {
                    transfer.down = Some(if target.transport == ThroughputTransport::FetchStream {
                        Download::start(
                            transport.clone(),
                            lanes(false),
                            operation_limit,
                            stopped.clone(),
                        )
                        .await?
                    } else {
                        Download::start_webtransport(
                            http,
                            target,
                            lanes(false),
                            operation_limit,
                            config.insecure,
                            stopped.clone(),
                        )
                        .await?
                    });
                }
                if stage.uploads() {
                    transfer.up = Some(if target.transport == ThroughputTransport::FetchStream {
                        Upload::start(transport.clone(), lanes(true), epoch, stopped.clone())
                            .await?
                    } else {
                        Upload::start_webtransport(
                            transport.clone(),
                            lanes(true),
                            epoch,
                            target.transport == ThroughputTransport::WebTransportDatagram,
                            stopped.clone(),
                        )
                        .await?
                    });
                }
            }
            resources.transfers.push(transfer);
        }
        // One primary latency stream measures the shared load.
        let latency_target = servers.first().and_then(|server| server.latency.clone());
        if stage == Stage::Latency && latency_target.is_none() {
            return Err("missing primary latency target".into());
        }
        if let Some(target) =
            latency_target.filter(|_| stage == Stage::Latency || config.loaded_latency)
        {
            let http = http.clone();
            let stopped = latency_stopped.clone();
            let observations = observations.clone();
            let insecure = config.insecure;
            let interval = config.ping_interval;
            resources.latency.spawn(async move {
                match target.transport {
                    LatencyTransport::WebSocket => {
                        crate::latency::run(
                            &http,
                            &target.base_url,
                            insecure,
                            interval,
                            operation_limit,
                            observations,
                            stopped,
                        )
                        .await
                    }
                    LatencyTransport::WebTransport => {
                        crate::webtransport::run_latency(
                            &http,
                            &target.base_url,
                            insecure,
                            interval,
                            operation_limit,
                            observations,
                            stopped,
                        )
                        .await
                    }
                }
            });
        }
        if !resources.latency.is_empty() {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    tokio::select! {
                        event = events.recv() => match event {
                            Some(Observation::Sample { .. }) => return Ok::<(), Error>(()),
                            Some(_) => {},
                            None => return Err("latency observations ended before readiness".into()),
                        },
                        task = resources.latency.join_next() => {
                            task.ok_or("missing latency task")???;
                            return Err("latency session ended before readiness".into());
                        }
                    }
                }
            }).await??;
        }
        snapshots.send_modify(|snapshot| {
            snapshot.phase = Phase::Warmup;
            snapshot.status = "Warming up".into();
        });
        let warmup_end = Instant::now() + config.warmup;
        loop {
            resources.health()?;
            tokio::select! {
                _ = tokio::time::sleep_until(warmup_end) => break,
                _ = events.recv() => {},
            }
        }
        let started = Instant::now();
        let end = started + config.duration(stage);
        measurement_start = Some(started);
        if let Some(stage) = transfer_stage {
            accounting.begin(
                stage,
                servers
                    .iter()
                    .map(|server| server.entry.id.clone())
                    .collect(),
                nanos(epoch.elapsed()),
                IntervalReason::StageStart,
            );
            let initial = resources.boundary(epoch);
            tokio::pin!(initial);
            loop {
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(end) => return Err("initial receiver checkpoint exceeded stage deadline".into()),
                    event = events.recv() => {
                        if let Some(event) = event {
                            observe_latency(event, started, end, &mut latency, &mut latest_latency);
                        }
                    },
                    boundary = &mut initial => {
                        accounting.observe(boundary?);
                        break;
                    }
                }
            }
        }
        let mut sample = tokio::time::interval(Duration::from_millis(500));
        sample.set_missed_tick_behavior(MissedTickBehavior::Skip);
        sample.tick().await;
        snapshots.send_modify(|snapshot| {
            snapshot.phase = Phase::Measuring;
            snapshot.status = format!("Measuring {}", stage.name());
        });
        loop {
            if let Err(error) = resources.health() {
                resources.recover(error, &mut accounting, transfer_stage, epoch, snapshots)?;
            }
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(end) => break,
                event = events.recv() => {
                    if let Some(event) = event { observe_latency(event, started, end, &mut latency, &mut latest_latency); }
                },
                _ = sample.tick() => {
                    let window = if transfer_stage.is_some() {
                        let boundary = {
                            let checkpoint = resources.boundary(epoch);
                            tokio::pin!(checkpoint);
                            loop {
                                tokio::select! {
                                    biased;
                                    _ = tokio::time::sleep_until(end) => break None,
                                    event = events.recv() => {
                                        if let Some(event) = event {
                                            observe_latency(event, started, end, &mut latency, &mut latest_latency);
                                        }
                                    },
                                    result = &mut checkpoint => break Some(result),
                                }
                            }
                        };
                        let Some(boundary) = boundary else { break; };
                        match boundary {
                            Ok(boundary) => accounting.observe(boundary),
                            Err(error) => {
                                resources.recover(error, &mut accounting, transfer_stage, epoch, snapshots)?;
                                None
                            }
                        }
                    } else { None };
                    snapshots.send_modify(|snapshot| snapshot.sample(Point {
                        elapsed: started.elapsed(),
                        down_bps: window.as_ref().and_then(|window| window.down_bytes_per_sec).map(|rate| rate * 8.0),
                        up_bps: window.as_ref().and_then(|window| window.up_bytes_per_sec).map(|rate| rate * 8.0),
                        latency_ms: latest_latency.take(),
                    }));
                }
            }
        }
        measurement_end = Some(end);
        resources.stop_latency.send_replace(true);
        if transfer_stage.is_some() {
            loop {
                match resources.boundary(epoch).await {
                    Ok(boundary) => {
                        accounting.observe(boundary);
                        break;
                    }
                    Err(error) => resources.recover(
                        error,
                        &mut accounting,
                        transfer_stage,
                        epoch,
                        snapshots,
                    )?,
                }
            }
        }
        resources.stop.send_replace(true);
        while let Some(result) = resources.latency.join_next().await {
            result??;
        }
        while let Ok(event) = events.try_recv() {
            observe_latency(event, started, end, &mut latency, &mut latest_latency);
        }
        Ok::<(), Error>(())
    };
    let result = tokio::select! {
        result = operation => result,
        _ = cancel.wait_for(|value| *value) => Ok(()),
    };
    let stopped_at = Instant::now();
    measurement_end.get_or_insert(stopped_at);
    if measurement_start.is_some() && (result.is_err() || *cancel.borrow()) {
        // Preserve received bytes even when cancellation prevents a final remote
        // checkpoint. Missing receiver windows remain explicitly incomplete.
        accounting.observe(resources.local_boundary(epoch));
    }
    resources.stop_latency.send_replace(true);
    let mut result = result;
    while let Some(joined) = resources.latency.join_next().await {
        if let Err(error) = joined.map_err(Error::from).and_then(|value| value)
            && result.is_ok()
        {
            result = Err(error);
        }
    }
    if let Some(started) = measurement_start {
        let ended = measurement_end.unwrap_or_else(Instant::now);
        while let Ok(event) = events.try_recv() {
            observe_latency(event, started, ended, &mut latency, &mut latest_latency);
        }
        let down = transfer_stage.map(|stage| accounting.result(stage, Direction::Down));
        let up = transfer_stage.map(|stage| accounting.result(stage, Direction::Up));
        snapshots.send_modify(|snapshot| {
            snapshot.results.push(StageResult {
                stage,
                elapsed: ended.duration_since(started),
                down_bytes: down.as_ref().map_or(0, |result| result.total_bytes),
                up_bytes: up.as_ref().map_or(0, |result| result.total_bytes),
                down_bps: down
                    .as_ref()
                    .and_then(|result| result.mean_bytes_per_sec)
                    .map(|rate| rate * 8.0),
                up_bps: up
                    .as_ref()
                    .and_then(|result| result.mean_bytes_per_sec)
                    .map(|rate| rate * 8.0),
                latency: latency.snapshot(),
                complete: result.is_ok()
                    && resources.failed.is_empty()
                    && !resources.latency_failed
                    && !*cancel.borrow()
                    && (stage != Stage::Latency || latency.snapshot().count > 0)
                    && (!stage.downloads()
                        || down.is_some_and(|result| result.mean_bytes_per_sec.is_some()))
                    && (!stage.uploads()
                        || up.is_some_and(|result| result.mean_bytes_per_sec.is_some())),
            })
        });
    }
    let failed = resources.failed.clone();
    let cleanup = resources.close().await;
    if cleanup.is_err() {
        snapshots.send_modify(|snapshot| {
            if let Some(last) = snapshot.results.last_mut()
                && last.stage == stage
            {
                last.complete = false;
            }
        });
    }
    result.and(cleanup).map(|()| failed)
}

fn observe_latency(
    event: Observation,
    start: Instant,
    end: Instant,
    accumulator: &mut LatencyAccumulator,
    latest: &mut Option<f64>,
) {
    let sent = match event {
        Observation::ConnectionBoundary => {
            accumulator.break_continuity();
            return;
        }
        Observation::Sample { sent, .. } | Observation::Lost { sent, .. } => sent,
    };
    if sent < start || sent >= end {
        return;
    }
    if let Observation::Sample { received, .. } = event
        && received >= end
    {
        accumulator.record(graphite_meter_core::latency::ProbeOutcome::Unresolved);
        return;
    }
    if let Observation::Sample { rtt, .. } = event {
        *latest = Some(rtt.as_secs_f64() * 1000.0);
    }
    if let Some(outcome) = event.outcome() {
        accumulator.record(outcome);
    }
}

fn stream_count(config: &Config, target: &ThroughputTarget, servers: usize, upload: bool) -> usize {
    if config.streams > 0 {
        return config.streams;
    }
    let desired = match (target.transport, target.protocol, upload) {
        (ThroughputTransport::FetchStream, Protocol::Http1 | Protocol::Negotiated, _) => {
            config.auto_streams
        }
        (ThroughputTransport::FetchStream, Protocol::Http2, true) => 4,
        _ => 1,
    };
    desired.min(128 / servers.max(1)).max(1)
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_after_stage_end_remain_unresolved() {
        let start = Instant::now();
        let end = start + Duration::from_secs(1);
        let mut accumulator = LatencyAccumulator::default();
        let mut latest = None;
        observe_latency(
            Observation::Sample {
                sent: end - Duration::from_millis(10),
                received: end + Duration::from_millis(10),
                rtt: Duration::from_millis(20),
                server_handling: Duration::ZERO,
            },
            start,
            end,
            &mut accumulator,
            &mut latest,
        );
        let summary = accumulator.snapshot();
        assert_eq!(summary.count, 0);
        assert_eq!(summary.unresolved, 1);
        assert_eq!(latest, None);
    }

    #[test]
    fn warmup_and_poststage_probes_do_not_enter_measurement() {
        let start = Instant::now();
        let end = start + Duration::from_secs(1);
        let mut accumulator = LatencyAccumulator::default();
        let mut latest = None;
        for sent in [start - Duration::from_millis(10), end] {
            observe_latency(
                Observation::Sample {
                    sent,
                    received: sent + Duration::from_millis(1),
                    rtt: Duration::from_millis(1),
                    server_handling: Duration::ZERO,
                },
                start,
                end,
                &mut accumulator,
                &mut latest,
            );
        }
        assert!(!accumulator.snapshot().has_observations());
    }
}
