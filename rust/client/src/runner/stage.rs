//! One stage owns its members' transfers and latency sessions, their accounting and their cleanup.
use super::{PreparedServer, ServerError};
use crate::{
    Error,
    config::Config,
    download::Download,
    failure::MeasurementFailure,
    latency::{Observation, Stop},
    model::{
        Ending, FailureScope, Phase, Point, ServerContribution, ServerLatency, ServerLatencyResult, Snapshot, Stage,
        StageResult,
    },
    transport::{TRANSFER_PROGRESS_TIMEOUT, Transport},
    upload::Upload,
};
use futures_util::{
    FutureExt, StreamExt,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered, SelectAll},
};
use graphite_meter_core::{
    discovery::{Protocol, ThroughputTransport},
    failure::FailureReason,
    latency::LatencyAccumulator,
    measurement::{
        AggregateMeasurements, AggregateWindow, Boundary, CHECKPOINT_BUDGET, CLIENT_STALL, Direction,
        FINAL_CHECKPOINT_BUDGET, SAMPLE_INTERVAL, Stage as TransferStage,
    },
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};

const STAGE_READY_TIMEOUT: Duration = Duration::from_secs(10);
const STALL_QUIET: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Lanes {
    down: Option<Download>,
    up: Option<Upload>,
}

impl Lanes {
    async fn close(self, confirm: bool) -> Result<(), Error> {
        if let Some(down) = self.down {
            down.stop().await;
        }
        if let Some(up) = self.up {
            up.finish(confirm).await?;
        }
        Ok(())
    }
}

/// A selected server in this stage; it leaves only through `StageRun::fail`.
struct Member {
    id: String,
    stop: watch::Sender<bool>,
    latency: Option<watch::Sender<Stop>>,
    starting: bool,
    dialled: bool,
    latency_failed: bool,
    lanes: Lanes,
    checkpoint_misses: u8,
    moved: [Instant; 2],
}

impl Member {
    fn dialling(&self) -> bool {
        self.latency.is_some() && !self.dialled && !self.latency_failed
    }

    fn stop_latency(&self, stop: Stop) {
        if let Some(latency) = &self.latency {
            latency.send_if_modified(|current| {
                let raised = *current < stop;
                *current = (*current).max(stop);
                raised
            });
        }
    }

    fn health(&mut self) -> Result<(), Error> {
        if let Some(down) = &mut self.lanes.down {
            down.health()?;
        }
        if let Some(up) = &self.lanes.up {
            up.health()?;
        }
        Ok(())
    }

    /// A refused grant leaves at once; three missed checkpoints in a row leave, except at the final boundary.
    fn missed(&mut self, error: Option<Error>, final_boundary: bool) -> Option<Error> {
        let Some(error) = error else {
            self.checkpoint_misses = 0;
            return None;
        };
        self.checkpoint_misses += 1;
        (crate::net::authentication_required(error.as_ref()).is_some()
            || self.checkpoint_misses >= 3 && !final_boundary)
            .then_some(error)
    }
}

struct LatencyCompletion {
    id: String,
    at: Instant,
    stopped: bool,
    result: Result<(), Error>,
}

#[derive(Default)]
struct HostLatency {
    accumulator: LatencyAccumulator,
    latest: Option<f64>,
    ended_at: Option<Instant>,
    ending: Option<Ending>,
}

type Start<'a> = BoxFuture<'a, (String, Result<Lanes, Error>)>;

struct StageRun<'a> {
    stage: Stage,
    transfer: Option<TransferStage>,
    config: &'a Config,
    snapshots: &'a watch::Sender<Snapshot>,
    epoch: Instant,
    participants: Vec<String>,
    members: Vec<Member>,
    starts: FuturesUnordered<Start<'a>>,
    latency: JoinSet<LatencyCompletion>,
    events: SelectAll<BoxStream<'static, (String, Observation)>>,
    hosts: BTreeMap<String, HostLatency>,
    retired: JoinSet<()>,
    removed: Vec<String>,
    lost: Option<ServerError>,
    accounting: AggregateMeasurements,
    window: Option<(Instant, Instant)>,
}

pub(super) async fn measure(
    stage: Stage,
    config: &Config,
    servers: &[PreparedServer],
    snapshots: &watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
) -> Result<Vec<String>, Error> {
    let mut run = StageRun::open(stage, config, servers, snapshots)?;
    let result = tokio::select! {
        result = run.run(servers) => result,
        _ = cancel.wait_for(|value| *value) => Ok(()),
    };
    let stopped = *cancel.borrow();
    run.close(result, stopped).await
}

impl<'a> StageRun<'a> {
    fn open(
        stage: Stage,
        config: &'a Config,
        servers: &'a [PreparedServer],
        snapshots: &'a watch::Sender<Snapshot>,
    ) -> Result<Self, Error> {
        let planned_warmup = servers.iter().fold(config.warmup, |warmup, server| {
            warmup.max(adaptive_warmup(config.warmup, server.idle_rtt))
        });
        let operation_limit = planned_warmup
            .checked_add(config.duration(stage))
            .and_then(|duration| duration.checked_add(Duration::from_secs(60)))
            .ok_or("stage duration overflow")?;
        snapshots.send_modify(|snapshot| {
            snapshot.phase = Phase::Preparing;
            snapshot.stage = Some(stage);
            snapshot.latest = Point::default();
            let offset = snapshot.results.iter().map(|result| result.elapsed).sum::<Duration>();
            snapshot.history.add(Point {
                elapsed: offset,
                ..Point::default()
            });
            let mut previous = std::mem::take(&mut snapshot.server_latencies);
            snapshot.server_latencies = servers
                .iter()
                .map(|server| {
                    let mut history = previous
                        .iter_mut()
                        .find(|host| host.id == server.entry.id)
                        .map(|host| std::mem::take(&mut host.history))
                        .unwrap_or_default();
                    history.add(Point {
                        elapsed: offset,
                        ..Point::default()
                    });
                    ServerLatency {
                        id: server.entry.id.clone(),
                        history,
                        ..ServerLatency::default()
                    }
                })
                .collect();
        });
        let epoch = Instant::now();
        let mut run = Self {
            stage,
            transfer: match stage {
                Stage::Latency => None,
                Stage::Download => Some(TransferStage::Download),
                Stage::Upload => Some(TransferStage::Upload),
                Stage::Bidirectional => Some(TransferStage::Bidirectional),
            },
            config,
            snapshots,
            epoch,
            participants: servers.iter().map(|server| server.entry.id.clone()).collect(),
            members: Vec::new(),
            starts: FuturesUnordered::new(),
            latency: JoinSet::new(),
            events: SelectAll::new(),
            hosts: BTreeMap::new(),
            retired: JoinSet::new(),
            removed: Vec::new(),
            lost: None,
            accounting: AggregateMeasurements::default(),
            window: None,
        };
        for server in servers {
            let (stop, stopped) = watch::channel(false);
            let latency = (stage == Stage::Latency || config.loaded_latency)
                .then(|| run.spawn_latency(server, operation_limit))
                .transpose()?;
            if run.transfer.is_some() {
                let id = server.entry.id.clone();
                let start = start_transfer(stage, server, config, operation_limit, stopped);
                run.starts.push(start.map(move |started| (id, started)).boxed());
            }
            run.members.push(Member {
                id: server.entry.id.clone(),
                stop,
                latency,
                starting: run.transfer.is_some(),
                dialled: false,
                latency_failed: false,
                lanes: Lanes::default(),
                checkpoint_misses: 0,
                moved: [epoch; 2],
            });
        }
        Ok(run)
    }

    fn spawn_latency(
        &mut self,
        server: &PreparedServer,
        operation_limit: Duration,
    ) -> Result<watch::Sender<Stop>, Error> {
        let target = server.latency.clone().ok_or("missing selected latency target")?;
        let id = server.entry.id.clone();
        let http = server.client.clone();
        let (stop, stopped) = watch::channel(Stop::Running);
        // Each transport can settle up to 256 unresolved probes at once.
        // Keep headroom for observations queued during receiver checkpoints.
        let (observations, receiver) = mpsc::channel(1024);
        self.events.push(
            futures_util::stream::unfold((id.clone(), receiver), |(id, mut receiver)| async {
                receiver.recv().await.map(|event| ((id.clone(), event), (id, receiver)))
            })
            .boxed(),
        );
        self.hosts.insert(id.clone(), HostLatency::default());
        let insecure = self.config.insecure;
        let idle = self.stage == Stage::Latency;
        let interval = if idle {
            self.config.ping_interval
        } else {
            self.config.loaded_ping_interval
        };
        let window = if !idle {
            2
        } else if interval.is_zero() {
            4
        } else {
            16
        };
        self.latency.spawn(async move {
            let result = crate::latency::run(
                &http,
                &target,
                insecure,
                (interval, operation_limit, window),
                observations,
                stopped.clone(),
            )
            .await;
            LatencyCompletion {
                at: Instant::now(),
                stopped: *stopped.borrow() != Stop::Running,
                result,
                id,
            }
        });
        Ok(stop)
    }

    async fn run(&mut self, servers: &[PreparedServer]) -> Result<(), Error> {
        self.ready().await?;
        self.warmup(servers).await?;
        self.open_window().await?;
        self.measure_window().await?;
        self.finish_window().await
    }

    /// Transfers and latency sessions share one readiness budget; latency is ready once dialled.
    async fn ready(&mut self) -> Result<(), Error> {
        let ready_by = Instant::now() + STAGE_READY_TIMEOUT;
        let mut expired = false;
        while !self.starts.is_empty() || self.members.iter().any(Member::dialling) {
            tokio::select! {
                Some((id, started)) = self.starts.next(), if !self.starts.is_empty() => self.started(&id, started)?,
                Some(event) = self.events.next(), if !self.events.is_empty() => self.observe_latency(event),
                Some(joined) = self.latency.join_next(), if !self.latency.is_empty() => self.latency_ended(joined)?,
                () = tokio::time::sleep_until(ready_by), if !expired => {
                    expired = true;
                    let late: Vec<_> = self
                        .members
                        .iter()
                        .filter(|member| member.starting || member.dialling())
                        .map(|member| (member.id.clone(), member.starting))
                        .collect();
                    let mut removed = false;
                    for (id, starting) in late {
                        let error: Error = std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "server resources were not ready within 10 seconds",
                        )
                        .into();
                        let scope = if starting { FailureScope::Throughput } else { FailureScope::Latency };
                        removed |= self.fail(&id, scope, error, true);
                    }
                    self.settle(removed)?;
                },
            }
        }
        Ok(())
    }

    fn started(&mut self, id: &str, started: Result<Lanes, Error>) -> Result<(), Error> {
        let Some(member) = self.members.iter_mut().find(|member| member.id == id) else {
            if let Ok(lanes) = started {
                self.retire(lanes);
            }
            return Ok(());
        };
        member.starting = false;
        match started {
            Ok(lanes) => {
                member.lanes = lanes;
                Ok(())
            }
            Err(error) => {
                let removed = self.fail(id, FailureScope::Throughput, error, true);
                self.settle(removed)
            }
        }
    }

    fn retire(&mut self, lanes: Lanes) {
        self.retired.spawn(async move {
            let _ = lanes.close(false).await;
        });
    }

    async fn warmup(&mut self, servers: &[PreparedServer]) -> Result<(), Error> {
        let warmup = servers
            .iter()
            .filter(|server| self.members.iter().any(|member| member.id == server.entry.id))
            .fold(self.config.warmup, |warmup, server| {
                warmup.max(adaptive_warmup(self.config.warmup, server.idle_rtt))
            });
        self.snapshots.send_modify(|snapshot| snapshot.phase = Phase::Warmup);
        let end = Instant::now() + warmup;
        loop {
            self.check_health()?;
            tokio::select! {
                () = tokio::time::sleep_until(end) => return Ok(()),
                Some(event) = self.events.next(), if !self.events.is_empty() => self.observe_latency(event),
                Some(joined) = self.latency.join_next(), if !self.latency.is_empty() => self.latency_ended(joined)?,
            }
        }
    }

    async fn open_window(&mut self) -> Result<(), Error> {
        let initial = match self.transfer {
            Some(_) => {
                let (initial, misses) = self.collect(CHECKPOINT_BUDGET, None).await.expect("no stage end");
                self.depart(misses.into_iter().collect(), true)?;
                self.check_health()?;
                Some(initial)
            }
            None => None,
        };
        let started = Instant::now();
        self.window = Some((started, started + self.config.duration(self.stage)));
        if let (Some(stage), Some(mut initial)) = (self.transfer, initial) {
            // Local counters restart at the actual measurement start.
            let local = self.local_boundary();
            initial.at_nanos = local.at_nanos;
            initial.down = local.down;
            initial.observed_up = local.observed_up;
            let participants = self.members.iter().map(|member| member.id.clone()).collect();
            self.accounting.begin_stage(stage, participants, initial.at_nanos);
            self.accounting.observe(initial);
        }
        for member in &mut self.members {
            member.moved = [started; 2];
        }
        Ok(())
    }

    async fn measure_window(&mut self) -> Result<(), Error> {
        let (started, end) = self.window.expect("window opened");
        self.snapshots.send_modify(|snapshot| snapshot.phase = Phase::Measuring);
        let mut sample = tokio::time::interval(SAMPLE_INTERVAL);
        sample.set_missed_tick_behavior(MissedTickBehavior::Skip);
        sample.tick().await;
        loop {
            self.check_health()?;
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(end) => return Ok(()),
                Some(event) = self.events.next(), if !self.events.is_empty() => self.observe_latency(event),
                Some(joined) = self.latency.join_next(), if !self.latency.is_empty() => self.latency_ended(joined)?,
                scheduled = sample.tick() => {
                    let stalled = scheduled.elapsed() > CLIENT_STALL;
                    let window = match self.transfer {
                        Some(_) => {
                            let Some((mut boundary, misses)) = self.collect(CHECKPOINT_BUDGET, Some(end)).await else {
                                return Ok(());
                            };
                            boundary.stalled = stalled;
                            self.observe_boundary(boundary, misses)?
                        }
                        None => None,
                    };
                    self.publish(started, window.as_ref());
                },
            }
        }
    }

    async fn finish_window(&mut self) -> Result<(), Error> {
        let ended = Instant::now();
        for member in &self.members {
            member.stop_latency(Stop::Drain);
        }
        if self.transfer.is_none() {
            return Ok(());
        }
        // As Go's final(): a transfer lost up to the final boundary leaves with its cause, and every
        // removal there collects a fresh final boundary for the servers that remain.
        loop {
            self.check_health()?;
            let remaining = self.members.len();
            let (mut boundary, misses) = self.collect(FINAL_CHECKPOINT_BUDGET, None).await.expect("no stage end");
            self.check_health()?;
            if self.members.len() < remaining {
                continue;
            }
            boundary.final_boundary = true;
            self.observe_boundary(boundary, misses)?;
            if self.members.len() == remaining {
                break;
            }
        }
        let retrying = self
            .members
            .iter()
            .filter_map(|member| {
                let lanes = [
                    member.lanes.down.as_ref().map(Download::retrying),
                    member.lanes.up.as_ref().map(Upload::retrying),
                ];
                lanes
                    .into_iter()
                    .zip(member.moved)
                    .find_map(|(failure, moved)| {
                        failure
                            .flatten()
                            .filter(|_| ended.saturating_duration_since(moved) >= STALL_QUIET)
                    })
                    .map(|failure| (member.id.clone(), failure))
            })
            .collect();
        self.depart(retrying, false)
    }

    fn check_health(&mut self) -> Result<(), Error> {
        let failures = self
            .members
            .iter_mut()
            .filter_map(|member| member.health().err().map(|error| (member.id.clone(), error)))
            .collect();
        self.depart(failures, false)
    }

    fn depart(&mut self, failures: Vec<(String, Error)>, preparing: bool) -> Result<(), Error> {
        let mut removed = false;
        for (id, error) in failures {
            removed |= self.fail(&id, FailureScope::Throughput, error, preparing);
        }
        self.settle(removed)
    }

    /// Records a failure once; a throughput failure, or a latency-stage loss beside another server, removes it.
    fn fail(&mut self, id: &str, scope: FailureScope, error: Error, preparing: bool) -> bool {
        let Some(index) = self.members.iter().position(|member| member.id == id) else {
            return false;
        };
        let reason = crate::failure::reason(error.as_ref(), preparing);
        let remaining = self.members.len();
        let member = &mut self.members[index];
        let removed = match scope {
            FailureScope::Latency if member.latency_failed => return false,
            FailureScope::Latency => {
                member.latency_failed = true;
                member.stop_latency(Stop::Now);
                self.stage == Stage::Latency
                    && matches!(reason, FailureReason::ConnectionLost | FailureReason::Timeout)
                    && remaining > 1
            }
            FailureScope::Throughput => true,
        };
        if let Some(host) = self.hosts.get_mut(id) {
            host.ending.get_or_insert(Ending::Failed(reason));
        }
        self.snapshots.send_modify(|snapshot| {
            snapshot.failure(id, scope, &error);
            if let Some(latency) = snapshot.server_latencies.iter_mut().find(|latency| latency.id == id) {
                latency.latest_ms = None;
            }
            if removed {
                snapshot.leave(id);
            }
        });
        if removed {
            let member = self.members.remove(index);
            member.stop.send_replace(true);
            member.stop_latency(Stop::Now);
            self.retire(member.lanes);
            self.removed.push(id.to_owned());
            self.lost = Some(ServerError {
                id: id.to_owned(),
                label: format!("all selected servers failed: {id}"),
                source: error,
            });
        }
        removed
    }

    /// Survivors restart their interval together after removals; with none left the stage ends.
    fn settle(&mut self, removed: bool) -> Result<(), Error> {
        if removed {
            let survivors: Vec<_> = self.members.iter().map(|member| member.id.clone()).collect();
            self.accounting.dropout(&survivors, nanos(self.epoch.elapsed()));
        }
        if !self.members.is_empty() {
            return Ok(());
        }
        Err(self
            .lost
            .take()
            .map_or_else(|| "no selected server remained".into(), Into::into))
    }

    fn latency_ended(&mut self, joined: Result<LatencyCompletion, tokio::task::JoinError>) -> Result<(), Error> {
        let completion = joined?;
        if let Some(host) = self.hosts.get_mut(&completion.id) {
            host.ended_at = Some(completion.at);
        }
        let error = match completion.result {
            Ok(()) if completion.stopped => return Ok(()),
            Ok(()) => "latency session ended before stage boundary".into(),
            Err(error) => error,
        };
        let removed = self.fail(&completion.id, FailureScope::Latency, error, self.window.is_none());
        self.settle(removed)
    }

    fn observe_latency(&mut self, event: (String, Observation)) {
        if matches!(event.1, Observation::ConnectionBoundary)
            && let Some(member) = self.members.iter_mut().find(|member| member.id == event.0)
        {
            member.dialled = true;
        }
        observe(&mut self.hosts, self.window, event);
    }

    fn local_boundary(&self) -> Boundary {
        let mut boundary = Boundary {
            at_nanos: nanos(self.epoch.elapsed()),
            ..Boundary::default()
        };
        for member in &self.members {
            if let Some(down) = &member.lanes.down {
                boundary.down.insert(member.id.clone(), down.bytes());
            }
            if let Some(observed) = member.lanes.up.as_ref().and_then(Upload::observed) {
                boundary.observed_up.insert(member.id.clone(), observed);
            }
        }
        boundary
    }

    /// Snapshots every local counter before waiting on any remote clock; parallel checkpoints keep one server's
    /// RTT from shifting its peers. With `until`, the stage end abandons the checkpoints.
    async fn collect(
        &mut self,
        budget: Duration,
        until: Option<Instant>,
    ) -> Option<(Boundary, BTreeMap<String, Error>)> {
        let mut boundary = self.local_boundary();
        let checkpoints = futures_util::future::join_all(self.members.iter().filter_map(|member| {
            let up = member.lanes.up.as_ref()?;
            Some(async move { (member.id.clone(), up.checkpoint(budget).await) })
        }));
        tokio::pin!(checkpoints);
        let results = loop {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(until.unwrap_or_else(Instant::now)), if until.is_some() => return None,
                Some(event) = self.events.next(), if !self.events.is_empty() => observe(&mut self.hosts, self.window, event),
                results = &mut checkpoints => break results,
            }
        };
        let mut misses = BTreeMap::new();
        for (id, result) in results {
            match result {
                Ok(snapshot) => {
                    boundary.up.insert(id, snapshot);
                }
                Err(error) => {
                    misses.insert(id, error);
                }
            }
        }
        Some((boundary, misses))
    }

    /// A member leaves when a direction's measured bytes stop growing for the silence limit, at every boundary.
    fn observe_boundary(
        &mut self,
        boundary: Boundary,
        mut misses: BTreeMap<String, Error>,
    ) -> Result<Option<AggregateWindow>, Error> {
        let final_boundary = boundary.final_boundary;
        let collected = self.epoch + Duration::from_nanos(boundary.at_nanos);
        let directions: &[Direction] = match self.transfer {
            Some(TransferStage::Download) => &[Direction::Down],
            Some(TransferStage::Upload) => &[Direction::Up],
            _ => &[Direction::Down, Direction::Up],
        };
        let before: Vec<Vec<u64>> = self
            .members
            .iter()
            .map(|member| {
                directions
                    .iter()
                    .map(|direction| self.accounting.bytes(&member.id, *direction))
                    .collect()
            })
            .collect();
        let window = self.accounting.observe(boundary);
        let mut departures = Vec::new();
        for (member, before) in self.members.iter_mut().zip(before) {
            if let Some(error) = member.missed(misses.remove(&member.id), final_boundary) {
                departures.push((member.id.clone(), error));
                continue;
            }
            for (direction, before) in directions.iter().zip(before) {
                let moved = &mut member.moved[*direction as usize];
                if self.accounting.bytes(&member.id, *direction) > before {
                    *moved = collected;
                } else if collected.saturating_duration_since(*moved) >= TRANSFER_PROGRESS_TIMEOUT {
                    let stalled: Error = Box::new(MeasurementFailure(FailureReason::Timeout));
                    departures.push((member.id.clone(), stalled));
                    break;
                }
            }
        }
        self.depart(departures, false)?;
        Ok(window)
    }

    fn publish(&mut self, started: Instant, window: Option<&AggregateWindow>) {
        let elapsed = started.elapsed();
        let hosts = &mut self.hosts;
        self.snapshots.send_modify(|snapshot| {
            sample_hosts(hosts, snapshot, elapsed);
            snapshot.sample(Point {
                elapsed,
                sample_count: 1,
                down_bps: window
                    .and_then(|window| window.down_bytes_per_sec)
                    .map(|rate| rate * 8.0),
                up_bps: window.and_then(|window| window.up_bytes_per_sec).map(|rate| rate * 8.0),
                latency_ms: None,
            });
        });
    }

    /// Stops and joins every resource before recording the stage; a stop never waits for probe deadlines.
    async fn close(mut self, result: Result<(), Error>, stopped: bool) -> Result<Vec<String>, Error> {
        for member in &self.members {
            member.stop.send_replace(true);
            member.stop_latency(if stopped { Stop::Now } else { Stop::Drain });
        }
        while let Some((id, started)) = self.starts.next().await {
            match (started, self.members.iter_mut().find(|member| member.id == id)) {
                (Ok(lanes), Some(member)) => member.lanes = lanes,
                (Ok(lanes), None) => self.retire(lanes),
                (Err(_), _) => {}
            }
        }
        let confirm = result.is_ok() && !stopped;
        let lanes = self
            .members
            .iter_mut()
            .map(|member| std::mem::take(&mut member.lanes).close(confirm));
        futures_util::future::join_all(lanes).await;
        while let Some(joined) = self.latency.join_next().await {
            if confirm {
                let _ = self.latency_ended(joined);
            } else if let Ok(completion) = joined
                && let Some(host) = self.hosts.get_mut(&completion.id)
            {
                host.ended_at = Some(completion.at);
            }
        }
        while let Some(Some(event)) = self.events.next().now_or_never() {
            observe(&mut self.hosts, self.window, event);
        }
        self.record(stopped);
        while self.retired.join_next().await.is_some() {}
        result.map(|()| self.removed)
    }

    fn record(&mut self, stopped: bool) {
        let measuring = self.window.is_some();
        let (started, end) = self.window.unwrap_or((self.epoch, self.epoch));
        let ended = end.min(Instant::now());
        let elapsed = ended.saturating_duration_since(started);
        let down = self
            .transfer
            .filter(|stage| measuring && stage.needs_down())
            .map(|_| self.accounting.result(Direction::Down));
        let up = self
            .transfer
            .filter(|stage| measuring && stage.needs_up())
            .map(|_| self.accounting.result(Direction::Up));
        let missing = (self.stage.downloads()
            && down.as_ref().is_none_or(|result| result.mean_bytes_per_sec.is_none()))
            || (self.stage.uploads() && up.as_ref().is_none_or(|result| result.mean_bytes_per_sec.is_none()));
        let hosts = &mut self.hosts;
        let accounting = &self.accounting;
        let (stage, transfer, members, participants) = (self.stage, self.transfer, &self.members, &self.participants);
        self.snapshots.send_modify(|snapshot| {
            if measuring {
                sample_hosts(hosts, snapshot, elapsed);
            }
            let server_latencies: Vec<_> = snapshot
                .server_latencies
                .iter()
                .map(|host| {
                    let own = hosts.get(&host.id);
                    ServerLatencyResult {
                        elapsed: own
                            .and_then(|own| own.ended_at)
                            .filter(|_| measuring)
                            .map(|at| at.min(ended).saturating_duration_since(started)),
                        id: host.id.clone(),
                        summary: own.map(|own| own.accumulator.snapshot()).unwrap_or_default(),
                        ending: own.and_then(|own| own.ending).or(stopped.then_some(Ending::Stopped)),
                    }
                })
                .collect();
            if measuring && !stopped {
                let insufficient: Error = Box::new(MeasurementFailure(FailureReason::InsufficientEvidence));
                let throughput_failed = snapshot
                    .failures
                    .iter()
                    .any(|failure| failure.stage == stage && failure.scope == FailureScope::Throughput);
                for member in members {
                    if missing && !throughput_failed {
                        snapshot.failure(&member.id, FailureScope::Throughput, &insufficient);
                    }
                    let unmeasured = server_latencies
                        .iter()
                        .any(|host| host.id == member.id && host.median().is_none());
                    if stage == Stage::Latency && unmeasured {
                        snapshot.failure(&member.id, FailureScope::Latency, &insufficient);
                    }
                }
            }
            let server_results = transfer
                .map(|transfer| {
                    participants
                        .iter()
                        .map(|id| ServerContribution {
                            id: id.clone(),
                            down: transfer
                                .needs_down()
                                .then(|| accounting.server_result(id, Direction::Down)),
                            up: transfer.needs_up().then(|| accounting.server_result(id, Direction::Up)),
                        })
                        .collect()
                })
                .unwrap_or_default();
            snapshot.results.push(StageResult {
                stage,
                elapsed,
                down,
                up,
                intervals: accounting.intervals().clone(),
                omitted_intervals: accounting.omitted_intervals(),
                stopped,
                server_latencies,
                server_results,
            });
            snapshot.refocus();
        });
    }
}

async fn start_transfer(
    stage: Stage,
    server: &PreparedServer,
    config: &Config,
    operation_limit: Duration,
    mut stopped: watch::Receiver<bool>,
) -> Result<Lanes, Error> {
    let target = server.throughput.as_ref().ok_or("missing throughput target")?;
    let transport = server.http.as_ref().ok_or("missing throughput connection")?;
    let (down, up) = config.lanes(target);
    let upload_transport = if stage == Stage::Bidirectional
        && target.transport == ThroughputTransport::FetchStream
        && target.protocol == Protocol::Http3
    {
        // Sustained downloads can occupy the connection send window
        // and starve upload control traffic at high lane counts.
        let connect = Transport::connect(
            server.client.clone(),
            &target.base_url,
            target.protocol,
            config.insecure,
        );
        tokio::select! {
            biased;
            _ = stopped.wait_for(|stopped| *stopped) => return Err("stage stopped before its lanes started".into()),
            connection = connect => Arc::new(connection?),
        }
    } else {
        transport.clone()
    };
    let mut lanes = Lanes::default();
    if stage.downloads() {
        lanes.down = Some(if target.transport == ThroughputTransport::FetchStream {
            Download::start(
                transport.clone(),
                down,
                operation_limit,
                lane_stagger(config.warmup, server.idle_rtt, down),
                stopped.clone(),
            )
            .await?
        } else {
            Download::start_webtransport(
                &server.client,
                target,
                down,
                operation_limit,
                config.insecure,
                stopped.clone(),
            )
            .await?
        });
    }
    if stage.uploads() {
        let upload = if target.transport == ThroughputTransport::FetchStream {
            let stagger = lane_stagger(config.warmup, server.idle_rtt, up);
            Upload::start(upload_transport, up, stagger, stopped).await
        } else {
            Upload::start_webtransport(upload_transport, up, stopped).await
        };
        match upload {
            Ok(upload) => lanes.up = Some(upload),
            Err(error) => {
                // A bidirectional member may have a live download when its upload cannot start.
                let _ = lanes.close(false).await;
                return Err(error);
            }
        }
    }
    Ok(lanes)
}

fn observe(
    hosts: &mut BTreeMap<String, HostLatency>,
    window: Option<(Instant, Instant)>,
    (id, event): (String, Observation),
) {
    if let (Some(host), Some((start, end))) = (hosts.get_mut(&id), window) {
        observe_latency(event, start, end, &mut host.accumulator, &mut host.latest);
    }
}

fn sample_hosts(hosts: &mut BTreeMap<String, HostLatency>, snapshot: &mut Snapshot, elapsed: Duration) {
    let offset = snapshot.results.iter().map(|result| result.elapsed).sum::<Duration>();
    for host in &mut snapshot.server_latencies {
        host.latest_ms = hosts.get_mut(&host.id).and_then(|state| state.latest.take());
        host.history.add(Point {
            elapsed: offset + elapsed,
            latency_ms: host.latest_ms,
            ..Point::default()
        });
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
