//! One stage owns its transfers, latency tasks, accounting and cleanup.
use super::{PreparedServer, lane_plan};
use crate::{
    Error,
    config::Config,
    download::Download,
    latency::Observation,
    model::{
        Phase, Point, ServerContribution, ServerLatency, ServerLatencyResult, Snapshot, Stage,
        StageResult,
    },
    stream_plan::StageLanePlan,
    transport::{TRANSFER_PROGRESS_TIMEOUT, Transport},
    upload::Upload,
};
use futures_util::{
    FutureExt, StreamExt,
    stream::{BoxStream, FuturesUnordered, SelectAll},
};
use graphite_meter_core::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    latency::LatencyAccumulator,
    measurement::{
        AggregateMeasurements, Boundary, CHECKPOINT_BUDGET, Direction, FINAL_CHECKPOINT_BUDGET,
        IntervalReason, SAMPLE_INTERVAL, Stage as TransferStage,
    },
};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};

#[derive(Clone, Copy)]
enum BoundaryKind {
    Initial,
    Sample,
    Final,
}

struct Transfer {
    id: String,
    down: Option<Download>,
    up: Option<Upload>,
    checkpoint_misses: u8,
}

struct StageResources {
    transfers: Vec<Transfer>,
    latency: JoinSet<Result<(), Error>>,
    stop: watch::Sender<bool>,
    stop_latency: BTreeMap<String, watch::Sender<bool>>,
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
struct AllParticipantsFailed(ParticipantFailure);
impl std::fmt::Display for AllParticipantsFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "all selected servers failed: {}", self.0)
    }
}
impl std::error::Error for AllParticipantsFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[derive(Debug)]
struct LatencyFailure {
    id: String,
    source: Error,
}
impl std::fmt::Display for LatencyFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} latency unavailable: {}",
            self.id, self.source
        )
    }
}
impl std::error::Error for LatencyFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn latency_task_result(
    id: String,
    result: Result<(), Error>,
    stop_requested: bool,
) -> Result<(), Error> {
    let source = match result {
        Ok(()) if stop_requested => return Ok(()),
        Ok(()) => "latency session ended before stage boundary".into(),
        Err(error) => error,
    };
    Err(LatencyFailure { id, source }.into())
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
    fn preparation_failure(
        &mut self,
        id: &str,
        error: &Error,
        snapshots: &watch::Sender<Snapshot>,
    ) {
        self.failed.push(id.to_owned());
        self.stop_host_latency(id);
        snapshots.send_modify(|snapshot| {
            if let Some(server) = snapshot.servers.iter_mut().find(|server| server.id == id) {
                server.error = Some(error.to_string());
            }
            if let Some(latency) = snapshot
                .server_latencies
                .iter_mut()
                .find(|latency| latency.id == id)
            {
                latency.error = Some("throughput participant unavailable".into());
                latency.latest_ms = None;
            }
            snapshot.status = format!("{id} unavailable; preparing remaining servers");
        });
    }

    fn stop_all_latency(&self) {
        for stop in self.stop_latency.values() {
            stop.send_replace(true);
        }
    }

    fn stop_host_latency(&self, id: &str) {
        if let Some(stop) = self.stop_latency.get(id) {
            stop.send_replace(true);
        }
    }

    async fn close(mut self) -> Result<(), Error> {
        self.stop.send_replace(true);
        self.stop_all_latency();
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
        while let Some(result) = self.latency.try_join_next() {
            result??;
        }
        Ok(())
    }

    fn record_latency_failure(
        &mut self,
        failure: &LatencyFailure,
        snapshots: &watch::Sender<Snapshot>,
    ) {
        self.latency_failed = true;
        self.stop_host_latency(&failure.id);
        snapshots.send_modify(|snapshot| {
            if let Some(latency) = snapshot
                .server_latencies
                .iter_mut()
                .find(|latency| latency.id == failure.id)
            {
                latency.error = Some(failure.source.to_string());
                latency.latest_ms = None;
            }
            snapshot.status = format!("{failure}; other measurements continue");
        });
    }

    fn latency_completion(
        &mut self,
        completed: Result<Result<(), Error>, tokio::task::JoinError>,
        snapshots: &watch::Sender<Snapshot>,
        latency_only: bool,
    ) -> Result<(), Error> {
        match completed? {
            Err(error) if error.is::<LatencyFailure>() => {
                let failure = error.downcast::<LatencyFailure>()?;
                self.record_latency_failure(&failure, snapshots);
                if latency_only && self.stop_latency.values().all(|stop| *stop.borrow()) {
                    return Err(AllParticipantsFailed(ParticipantFailure {
                        id: failure.id,
                        source: failure.source,
                    })
                    .into());
                }
                Ok(())
            }
            result => result,
        }
    }

    fn recover(
        &mut self,
        error: Error,
        accounting: &mut AggregateMeasurements,
        stage: Option<TransferStage>,
        measuring: bool,
        epoch: Instant,
        snapshots: &watch::Sender<Snapshot>,
    ) -> Result<(), Error> {
        if error.is::<LatencyFailure>() {
            let failure = error.downcast::<LatencyFailure>()?;
            self.record_latency_failure(&failure, snapshots);
            if stage.is_none() && self.stop_latency.values().all(|stop| *stop.borrow()) {
                return Err(AllParticipantsFailed(ParticipantFailure {
                    id: failure.id,
                    source: failure.source,
                })
                .into());
            }
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
        self.stop_host_latency(&failure.id);
        snapshots.send_modify(|snapshot| {
            if let Some(latency) = snapshot
                .server_latencies
                .iter_mut()
                .find(|latency| latency.id == failure.id)
            {
                latency.error = Some("throughput participant disconnected".into());
                latency.latest_ms = None;
            }
        });
        self.failed.push(failure.id.clone());
        self.retired.spawn(transfer.close());
        if self.transfers.is_empty() {
            return Err(AllParticipantsFailed(*failure).into());
        }
        if measuring && let Some(stage) = stage {
            accounting.begin(
                stage,
                self.transfers
                    .iter()
                    .map(|transfer| transfer.id.clone())
                    .collect(),
                nanos(epoch.elapsed()),
                IntervalReason::Dropout,
            );
            accounting.observe(self.local_boundary(epoch));
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

    async fn boundary(&mut self, epoch: Instant, kind: BoundaryKind) -> Result<Boundary, Error> {
        // Snapshot every local counter before waiting on any remote clock.
        // Parallel checkpoints prevent one server's RTT from shifting its peers.
        let mut boundary = self.local_boundary(epoch);
        let budget = if matches!(kind, BoundaryKind::Final) {
            FINAL_CHECKPOINT_BUDGET
        } else {
            CHECKPOINT_BUDGET
        };
        let checkpoints = self.transfers.iter_mut().filter_map(|transfer| {
            let Transfer {
                id,
                up,
                checkpoint_misses,
                ..
            } = transfer;
            up.as_ref().map(|up| async move {
                let result = up.checkpoint(budget).await;
                match result {
                    Ok(snapshot) => {
                        *checkpoint_misses = 0;
                        Ok::<_, Error>(Some((id.clone(), snapshot)))
                    }
                    Err(source) => {
                        *checkpoint_misses += 1;
                        if crate::net::authentication_required(source.as_ref()).is_some()
                            || matches!(kind, BoundaryKind::Initial)
                            || (matches!(kind, BoundaryKind::Sample) && *checkpoint_misses >= 3)
                        {
                            Err(ParticipantFailure {
                                id: id.clone(),
                                source,
                            }
                            .into())
                        } else {
                            Ok(None)
                        }
                    }
                }
            })
        });
        for result in futures_util::future::join_all(checkpoints).await {
            if let Some(snapshot) = result? {
                boundary.up.extend([snapshot]);
            }
        }
        Ok(boundary)
    }
}

fn server_contributions(
    stage: Option<TransferStage>,
    servers: &[PreparedServer],
    accounting: &AggregateMeasurements,
    snapshot: &Snapshot,
) -> Vec<ServerContribution> {
    let Some(stage) = stage else {
        return Vec::new();
    };
    servers
        .iter()
        .map(|server| {
            let id = &server.entry.id;
            let totals = accounting.stage_total_for_server(stage, id);
            ServerContribution {
                id: id.clone(),
                down_bytes: totals.down,
                up_bytes: totals.up,
                down_bps: accounting
                    .server_rate(stage, Direction::Down, id)
                    .map(|rate| rate * 8.0),
                up_bps: accounting
                    .server_rate(stage, Direction::Up, id)
                    .map(|rate| rate * 8.0),
                error: snapshot
                    .servers
                    .iter()
                    .find(|summary| summary.id == *id)
                    .and_then(|summary| summary.error.clone()),
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
struct StageTiming {
    epoch: Instant,
    operation_limit: Duration,
    setup_timeout: Duration,
}

async fn start_transfer(
    stage: Stage,
    server: &PreparedServer,
    plan: &StageLanePlan,
    config: &Config,
    timing: StageTiming,
    stopped: watch::Receiver<bool>,
) -> Result<Transfer, Error> {
    let mut transfer = Transfer {
        id: server.entry.id.clone(),
        down: None,
        up: None,
        checkpoint_misses: 0,
    };
    let started = tokio::time::timeout(timing.setup_timeout, async {
        if stage.downloads() || stage.uploads() {
            let target = server
                .throughput
                .as_ref()
                .ok_or("missing throughput target")?;
            let transport = server
                .http
                .as_ref()
                .ok_or("missing throughput connection")?;
            let lanes = plan
                .lanes(&server.entry.id)
                .ok_or("missing stream allocation")?;
            let upload_transport = if stage == Stage::Bidirectional
                && target.transport == ThroughputTransport::FetchStream
                && target.protocol == Protocol::Http3
            {
                // Sustained downloads can occupy the connection send window
                // and starve upload control traffic at high lane counts.
                Arc::new(
                    Transport::connect(
                        server.client.clone(),
                        &target.base_url,
                        target.protocol,
                        config.insecure,
                    )
                    .await?,
                )
            } else {
                transport.clone()
            };
            if stage.downloads() {
                transfer.down = Some(if target.transport == ThroughputTransport::FetchStream {
                    Download::start_staggered(
                        transport.clone(),
                        lanes.download,
                        timing.operation_limit,
                        lane_stagger(config.warmup, server.idle_rtt, lanes.download),
                        stopped.clone(),
                    )
                    .await?
                } else {
                    Download::start_webtransport(
                        &server.client,
                        target,
                        lanes.download,
                        timing.operation_limit,
                        config.insecure,
                        stopped.clone(),
                    )
                    .await?
                });
            }
            if stage.uploads() {
                transfer.up = Some(if target.transport == ThroughputTransport::FetchStream {
                    Upload::start_staggered(
                        upload_transport.clone(),
                        lanes.upload,
                        timing.epoch,
                        lane_stagger(config.warmup, server.idle_rtt, lanes.upload),
                        stopped.clone(),
                    )
                    .await?
                } else {
                    Upload::start_webtransport(
                        upload_transport,
                        lanes.upload,
                        timing.epoch,
                        target.transport == ThroughputTransport::WebTransportDatagram,
                        stopped.clone(),
                    )
                    .await?
                });
            }
        }
        Ok::<(), Error>(())
    })
    .await;
    let error = match started {
        Ok(Ok(())) => return Ok(transfer),
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    // A bidirectional peer may have a live download when upload setup fails.
    // This also runs when setup times out after the download became ready.
    let _ = transfer.close().await;
    Err(error)
}

pub(super) async fn measure(
    stage: Stage,
    config: &Config,
    servers: &[PreparedServer],
    snapshots: &watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
    completed_stage: bool,
) -> Result<Vec<String>, Error> {
    let plan = lane_plan(config, stage, servers)?;
    let planned_warmup = servers.iter().fold(config.warmup, |warmup, server| {
        warmup.max(adaptive_warmup(config.warmup, server.idle_rtt))
    });
    let epoch = Instant::now();
    let (stop, stopped) = watch::channel(false);
    let mut resources = StageResources {
        transfers: Vec::new(),
        latency: JoinSet::new(),
        stop,
        stop_latency: BTreeMap::new(),
        retired: JoinSet::new(),
        failed: Vec::new(),
        latency_failed: false,
    };
    let mut events: SelectAll<BoxStream<'static, (String, Observation)>> = SelectAll::new();
    let operation_limit = planned_warmup
        .checked_add(config.duration(stage))
        .and_then(|duration| duration.checked_add(Duration::from_secs(60)))
        .ok_or("stage duration overflow")?;
    snapshots.send_modify(|snapshot| {
        snapshot.phase = Phase::Preparing;
        snapshot.stage = Some(stage);
        snapshot.status = format!("Preparing {}", stage.name());
        snapshot.history.clear();
        snapshot.latest = Point::default();
        snapshot.server_latencies = if stage == Stage::Latency || config.loaded_latency {
            servers
                .iter()
                .map(|server| ServerLatency {
                    id: server.entry.id.clone(),
                    ..ServerLatency::default()
                })
                .collect()
        } else {
            Vec::new()
        };
    });
    let transfer_stage = match stage {
        Stage::Latency => None,
        Stage::Download => Some(TransferStage::Download),
        Stage::Upload => Some(TransferStage::Upload),
        Stage::Bidirectional => Some(TransferStage::Bidirectional),
    };
    let mut accounting = AggregateMeasurements::default();
    let mut latency = LatencyMeasurements::default();
    let mut measurement_start = None;
    let mut measurement_end = None;
    let operation = async {
        let mut last_failure = None;
        // Start every selected peer within the same preparation window. One
        // slow origin must not postpone another peer's first request.
        let mut starts = servers
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let stopped = stopped.clone();
                let plan = &plan;
                async move {
                    (
                        index,
                        start_transfer(
                            stage,
                            server,
                            plan,
                            config,
                            StageTiming {
                                epoch,
                                operation_limit,
                                setup_timeout: Duration::from_secs(12),
                            },
                            stopped,
                        )
                        .await,
                    )
                }
            })
            .collect::<FuturesUnordered<_>>();
        let mut started: Vec<Option<Result<Transfer, Error>>> =
            (0..servers.len()).map(|_| None).collect();
        while let Some((index, result)) = starts.next().await {
            started[index] = Some(result);
        }
        for (server, result) in servers.iter().zip(started) {
            match result.expect("every selected transfer preparation completed") {
                Ok(transfer) => resources.transfers.push(transfer),
                Err(error) => {
                    if !completed_stage {
                        last_failure.get_or_insert(ParticipantFailure {
                            id: server.entry.id.clone(),
                            source: error,
                        });
                        continue;
                    }
                    resources.preparation_failure(&server.entry.id, &error, snapshots);
                    if last_failure
                        .as_ref()
                        .is_none_or(|failure: &ParticipantFailure| {
                            crate::net::authentication_required(failure.source.as_ref()).is_none()
                        })
                        || crate::net::authentication_required(error.as_ref()).is_some()
                    {
                        last_failure = Some(ParticipantFailure {
                            id: server.entry.id.clone(),
                            source: error,
                        });
                    }
                }
            }
        }
        if !completed_stage && let Some(failure) = last_failure {
            return Err(failure.source);
        }
        if resources.transfers.is_empty() {
            return Err(AllParticipantsFailed(
                last_failure.expect("one preparation failure for each selected server"),
            )
            .into());
        }
        if stage == Stage::Latency || config.loaded_latency {
            for server in servers
                .iter()
                .filter(|server| !resources.failed.contains(&server.entry.id))
            {
                let target = server
                    .latency
                    .clone()
                    .ok_or("missing selected latency target")?;
                let id = server.entry.id.clone();
                let http = server.client.clone();
                let (stop, stopped) = watch::channel(false);
                resources.stop_latency.insert(id.clone(), stop);
                // Each transport can settle up to 256 unresolved probes at once.
                // Keep headroom for observations queued during receiver checkpoints.
                let (observations, receiver) = mpsc::channel(1024);
                events.push(
                    futures_util::stream::unfold(
                        (id.clone(), receiver),
                        |(id, mut receiver)| async {
                            receiver
                                .recv()
                                .await
                                .map(|event| ((id.clone(), event), (id, receiver)))
                        },
                    )
                    .boxed(),
                );
                latency.hosts.insert(id.clone(), HostLatency::default());
                let insecure = config.insecure;
                let interval = config.ping_interval;
                resources.latency.spawn(async move {
                    let result = match target.transport {
                        LatencyTransport::WebSocket => {
                            crate::latency::run(
                                &http,
                                &target.base_url,
                                insecure,
                                interval,
                                operation_limit,
                                observations,
                                stopped.clone(),
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
                                stopped.clone(),
                            )
                            .await
                        }
                    };
                    latency_task_result(id, result, *stopped.borrow())
                });
            }
            let mut ready = HashSet::new();
            let readiness = async {
                while resources.transfers.iter().any(|transfer| {
                    !ready.contains(&transfer.id)
                        && resources
                            .stop_latency
                            .get(&transfer.id)
                            .is_none_or(|stop| !*stop.borrow())
                }) {
                    tokio::select! {
                        event = events.next(), if !events.is_empty() => match event {
                            Some((id, Observation::Sample { .. })) => {
                                ready.insert(id);
                            },
                            Some(_) => {},
                            None => return Err("latency observations ended before readiness".into()),
                        },
                        task = resources.latency.join_next() => {
                            let task = task.ok_or("missing latency task")?;
                            if completed_stage {
                                resources.latency_completion(task, snapshots, stage == Stage::Latency)?;
                            } else {
                                task??;
                                return Err("latency session ended before readiness".into());
                            }
                        }
                    }
                }
                Ok::<(), Error>(())
            };
            match tokio::time::timeout(Duration::from_secs(12), readiness).await {
                Ok(result) => result?,
                Err(_) if completed_stage => {
                    let missing: Vec<_> = resources
                        .transfers
                        .iter()
                        .filter(|transfer| {
                            !ready.contains(&transfer.id)
                                && resources
                                    .stop_latency
                                    .get(&transfer.id)
                                    .is_some_and(|stop| !*stop.borrow())
                        })
                        .map(|transfer| transfer.id.clone())
                        .collect();
                    for id in missing {
                        resources.record_latency_failure(
                            &LatencyFailure {
                                id,
                                source: "latency session was not ready within 12 seconds".into(),
                            },
                            snapshots,
                        );
                    }
                    if stage == Stage::Latency
                        && resources.stop_latency.values().all(|stop| *stop.borrow())
                    {
                        return Err("all selected latency sessions failed".into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
            if stage == Stage::Latency && resources.stop_latency.values().all(|stop| *stop.borrow())
            {
                return Err("all selected latency sessions failed".into());
            }
        }
        let warmup = servers
            .iter()
            .filter(|server| !resources.failed.contains(&server.entry.id))
            .fold(config.warmup, |warmup, server| {
                warmup.max(adaptive_warmup(config.warmup, server.idle_rtt))
            });
        snapshots.send_modify(|snapshot| {
            snapshot.phase = Phase::Warmup;
            snapshot.status = "Warming up".into();
        });
        let warmup_end = Instant::now() + warmup;
        loop {
            if let Err(error) = resources.health() {
                if !completed_stage {
                    return Err(error);
                }
                resources.recover(
                    error,
                    &mut accounting,
                    transfer_stage,
                    false,
                    epoch,
                    snapshots,
                )?;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(warmup_end) => break,
                _ = events.next(), if !events.is_empty() => {},
            }
        }
        let baseline_deadline = Instant::now() + Duration::from_secs(12);
        let mut initial = None;
        if transfer_stage.is_some() {
            loop {
                let result = {
                    let checkpoint = resources.boundary(epoch, BoundaryKind::Initial);
                    tokio::pin!(checkpoint);
                    loop {
                        tokio::select! {
                            biased;
                            _ = tokio::time::sleep_until(baseline_deadline) => {
                                break Err("initial receiver checkpoint exceeded preparation deadline".into());
                            }
                            _ = events.next(), if !events.is_empty() => {},
                            boundary = &mut checkpoint => break boundary,
                        }
                    }
                };
                match result {
                    Ok(boundary) => {
                        initial = Some(boundary);
                        break;
                    }
                    Err(error) if completed_stage && error.is::<ParticipantFailure>() => {
                        resources.recover(
                            error,
                            &mut accounting,
                            transfer_stage,
                            false,
                            epoch,
                            snapshots,
                        )?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        let started = Instant::now();
        let end = started + config.duration(stage);
        measurement_start = Some(started);
        if let (Some(stage), Some(mut boundary)) = (transfer_stage, initial) {
            // Receiver snapshots retain their request/response brackets. Refresh
            // the local download counters at the actual measurement start.
            let local = resources.local_boundary(epoch);
            boundary.at_nanos = local.at_nanos;
            boundary.down = local.down;
            boundary.observed_up = local.observed_up;
            accounting.begin(
                stage,
                resources
                    .transfers
                    .iter()
                    .map(|transfer| transfer.id.clone())
                    .collect(),
                boundary.at_nanos,
                IntervalReason::StageStart,
            );
            accounting.observe(boundary);
        }
        let mut progress: BTreeMap<String, (u64, Instant)> = resources
            .transfers
            .iter()
            .map(|transfer| (transfer.id.clone(), (0, started)))
            .collect();
        let mut sample = tokio::time::interval(SAMPLE_INTERVAL);
        sample.set_missed_tick_behavior(MissedTickBehavior::Skip);
        sample.tick().await;
        snapshots.send_modify(|snapshot| {
            snapshot.phase = Phase::Measuring;
            snapshot.status = format!("Measuring {}", stage.name());
        });
        loop {
            if let Err(error) = resources.health() {
                resources.recover(
                    error,
                    &mut accounting,
                    transfer_stage,
                    true,
                    epoch,
                    snapshots,
                )?;
            }
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(end) => break,
                event = events.next(), if !events.is_empty() => {
                    if let Some(event) = event {
                        latency.observe(event, started, end);
                    }
                },
                _ = sample.tick() => {
                    let window = if transfer_stage.is_some() {
                        let boundary = {
                            let checkpoint = resources.boundary(epoch, BoundaryKind::Sample);
                            tokio::pin!(checkpoint);
                            loop {
                                tokio::select! {
                                    biased;
                                    _ = tokio::time::sleep_until(end) => break None,
                                    event = events.next(), if !events.is_empty() => {
                                        if let Some(event) = event {
                                            latency.observe(event, started, end);
                                        }
                                    },
                                    result = &mut checkpoint => break Some(result),
                                }
                            }
                        };
                        let Some(boundary) = boundary else { break; };
                        match boundary {
                            Ok(boundary) => {
                                let stalled = resources.transfers.iter().find_map(|transfer| {
                                    let bytes = boundary.down.get(&transfer.id).copied().unwrap_or_default()
                                        .saturating_add(boundary.observed_up.get(&transfer.id).map_or(0, |up| up.maximum)
                                            .max(boundary.up.get(&transfer.id).map_or(0, |up| up.bytes)));
                                    let (previous, last_progress) = progress.get_mut(&transfer.id).unwrap();
                                    if bytes != *previous {
                                        *previous = bytes;
                                        *last_progress = Instant::now();
                                    }
                                    (last_progress.elapsed() >= TRANSFER_PROGRESS_TIMEOUT).then(|| ParticipantFailure {
                                        id: transfer.id.clone(), source: "stopped delivering bytes".into(),
                                    })
                                });
                                let window = accounting.observe(boundary);
                                if let Some(failure) = stalled {
                                    resources.recover(failure.into(), &mut accounting, transfer_stage, true, epoch, snapshots)?;
                                    None
                                } else { window }
                            },
                            Err(error) => {
                                resources.recover(error, &mut accounting, transfer_stage, true, epoch, snapshots)?;
                                None
                            }
                        }
                    } else {
                        None
                    };
                    snapshots.send_modify(|snapshot| {
                        latency.sample(snapshot, started.elapsed());
                        snapshot.sample(Point {
                            elapsed: started.elapsed(),
                            down_bps: window.as_ref()
                                .and_then(|window| window.down_bytes_per_sec)
                                .map(|rate| rate * 8.0),
                            up_bps: window.as_ref()
                                .and_then(|window| window.up_bytes_per_sec)
                                .map(|rate| rate * 8.0),
                            latency_ms: snapshot.server_latencies.first()
                                .and_then(|host| host.latest_ms),
                        });
                    });
                }
            }
        }
        measurement_end = Some(end);
        resources.stop_all_latency();
        if transfer_stage.is_some() {
            loop {
                match resources.boundary(epoch, BoundaryKind::Final).await {
                    Ok(boundary) => {
                        accounting.observe(boundary);
                        break;
                    }
                    Err(error) => resources.recover(
                        error,
                        &mut accounting,
                        transfer_stage,
                        true,
                        epoch,
                        snapshots,
                    )?,
                }
            }
        }
        resources.stop.send_replace(true);
        while let Some(result) = resources.latency.join_next().await {
            resources.latency_completion(result, snapshots, stage == Stage::Latency)?;
        }
        while let Some(Some(event)) = events.next().now_or_never() {
            latency.observe(event, started, end);
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
    resources.stop_all_latency();
    let mut result = result;
    while let Some(joined) = resources.latency.join_next().await {
        if let Err(error) = resources.latency_completion(joined, snapshots, stage == Stage::Latency)
            && result.is_ok()
        {
            result = Err(error);
        }
    }
    if let Some(started) = measurement_start {
        let ended = measurement_end.unwrap_or_else(Instant::now);
        while let Some(Some(event)) = events.next().now_or_never() {
            latency.observe(event, started, ended);
        }
        let down = transfer_stage.map(|stage| accounting.result(stage, Direction::Down));
        let up = transfer_stage.map(|stage| accounting.result(stage, Direction::Up));
        snapshots.send_modify(|snapshot| {
            latency.sample(snapshot, ended.duration_since(started));
            let server_results =
                server_contributions(transfer_stage, servers, &accounting, snapshot);
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
                latency: snapshot
                    .server_latencies
                    .first()
                    .and_then(|host| latency.hosts.get(&host.id))
                    .map(|host| host.accumulator.snapshot())
                    .unwrap_or_default(),
                server_latencies: snapshot
                    .server_latencies
                    .iter()
                    .map(|host| ServerLatencyResult {
                        id: host.id.clone(),
                        summary: latency
                            .hosts
                            .get(&host.id)
                            .map(|host| host.accumulator.snapshot())
                            .unwrap_or_default(),
                        error: host.error.clone(),
                    })
                    .collect(),
                server_results,
                complete: result.is_ok()
                    && resources.failed.is_empty()
                    && !resources.latency_failed
                    && !*cancel.borrow()
                    && (stage != Stage::Latency
                        || latency
                            .hosts
                            .values()
                            .all(|host| host.accumulator.snapshot().count > 0))
                    && (!stage.downloads()
                        || down.is_some_and(|result| result.mean_bytes_per_sec.is_some()))
                    && (!stage.uploads()
                        || up.is_some_and(|result| result.mean_bytes_per_sec.is_some())),
            })
        });
    } else if result.is_err() && !*cancel.borrow() {
        // A failed preparation still belongs to this stage. Earlier completed
        // results remain untouched, and this missing population is explicit.
        snapshots.send_modify(|snapshot| {
            let server_results =
                server_contributions(transfer_stage, servers, &accounting, snapshot);
            snapshot.results.push(StageResult {
                stage,
                elapsed: Duration::ZERO,
                down_bytes: 0,
                up_bytes: 0,
                down_bps: None,
                up_bps: None,
                latency: LatencyAccumulator::default().snapshot(),
                complete: false,
                server_latencies: snapshot
                    .server_latencies
                    .iter()
                    .map(|host| ServerLatencyResult {
                        id: host.id.clone(),
                        summary: LatencyAccumulator::default().snapshot(),
                        error: host.error.clone(),
                    })
                    .collect(),
                server_results,
            });
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

#[derive(Default)]
struct HostLatency {
    accumulator: LatencyAccumulator,
    latest: Option<f64>,
}

#[derive(Default)]
struct LatencyMeasurements {
    hosts: BTreeMap<String, HostLatency>,
}
impl LatencyMeasurements {
    fn observe(&mut self, (id, event): (String, Observation), start: Instant, end: Instant) {
        if let Some(host) = self.hosts.get_mut(&id) {
            observe_latency(event, start, end, &mut host.accumulator, &mut host.latest);
        }
    }

    fn sample(&mut self, snapshot: &mut Snapshot, elapsed: Duration) {
        for host in &mut snapshot.server_latencies {
            host.latest_ms = self
                .hosts
                .get_mut(&host.id)
                .and_then(|state| state.latest.take());
            if host.history.len() == 300 {
                host.history.pop_front();
            }
            host.history.push_back((elapsed, host.latest_ms));
        }
    }
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

fn adaptive_warmup(base: Duration, rtt: Duration) -> Duration {
    base.max(rtt.saturating_mul(10)).min(Duration::from_secs(4))
}

fn lane_stagger(base: Duration, rtt: Duration, lanes: usize) -> Duration {
    if lanes <= 1 {
        Duration::ZERO
    } else {
        (adaptive_warmup(base, rtt) / 2 / (lanes - 1) as u32).min(Duration::from_millis(75))
    }
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests;
