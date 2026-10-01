//! One stage owns its members' transfers and latency sessions, their accounting and their cleanup.
use super::{PreparedServer, ServerError};
use crate::{
    Error,
    config::Config,
    download::Download,
    failure::Failure,
    latency::{Observation, Stop},
    model::{
        Ending, FailureScope, Phase, Point, ServerContribution, ServerLatencyResult, Snapshot, Stage, StageResult,
    },
    transport::{REDIAL_WINDOW, Transport},
    upload::Upload,
};
use futures_util::{
    FutureExt, StreamExt,
    future::{BoxFuture, OptionFuture},
    stream::{BoxStream, FuturesUnordered, SelectAll},
};
use graphite_meter_core::{
    discovery::{Protocol, ThroughputTransport},
    failure::FailureReason,
    latency::{LatencyAccumulator, ProbeOutcome},
    measurement::{
        AggregateMeasurements, AggregateWindow, Boundary, CHECKPOINT_BUDGET, CLIENT_STALL, Direction,
        FINAL_CHECKPOINT_BUDGET, MeasurementResult, SAMPLE_INTERVAL, Stage as TransferStage,
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
    /// The receiver starting in place of `up` after the server forgot its id.
    replacing: JoinSet<Result<Upload, Error>>,
}

impl Lanes {
    async fn close(mut self, confirm: bool) -> Result<(), Error> {
        if let Some(down) = self.down {
            down.stop().await;
        }
        // A replacement still starting ends on the member's stop.
        let replaced = self.replacing.join_next().await.and_then(|joined| joined.ok()?.ok());
        if let Some(up) = self.up.or(replaced) {
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

    /// As Go's measureUpload (upload.go:23-31), a receiver that no longer knows its upload id is
    /// replaced once per server and run; its new id resumes the aggregate's evidence.
    fn health(&mut self) -> Result<(), Error> {
        let lanes = &mut self.lanes;
        if let Some(down) = &mut lanes.down {
            down.health()?;
        }
        if let Some(replaced) = lanes.replacing.try_join_next() {
            lanes.up = Some(replaced??);
        }
        let Some(up) = &lanes.up else {
            return Ok(());
        };
        let Err(error) = up.health() else {
            return Ok(());
        };
        if !up.plan.replaces(&error) {
            return Err(error);
        }
        if let Some(up) = lanes.up.take() {
            lanes.replacing.spawn(up.replace());
        }
        Ok(())
    }

    /// A refused grant leaves at once; repeated misses leave only while another server moves.
    fn missed(&mut self, error: Option<Error>, final_boundary: bool, other_moving: bool) -> Option<Error> {
        let Some(error) = error else {
            self.checkpoint_misses = 0;
            return None;
        };
        self.checkpoint_misses = self.checkpoint_misses.saturating_add(1);
        (crate::failure::sign_in(error.as_ref()).is_some()
            || self.checkpoint_misses >= 3 && !final_boundary && other_moving)
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
    /// The probes in a row that timed out, which Go's live view reads out.
    timeouts: u32,
    /// What Go's chart takes of the probes measured since the last sample.
    steps: Vec<(Instant, f64, usize)>,
    ended_at: Option<Instant>,
    ending: Option<Ending>,
}

type Start<'a> = BoxFuture<'a, (String, Result<Lanes, Error>)>;

/// What Go's coordinator keeps across a run's stages: the start every boundary and failure is timed
/// from, and one aggregate account, so the interval history is capped per run.
pub(super) struct RunLedger {
    started: Instant,
    accounting: AggregateMeasurements,
}

impl RunLedger {
    pub(super) fn new() -> Self {
        Self {
            started: Instant::now(),
            accounting: AggregateMeasurements::default(),
        }
    }

    fn since_start(&self, at: Instant) -> Duration {
        at.saturating_duration_since(self.started)
    }
}

struct StageRun<'a> {
    stage: Stage,
    transfer: Option<TransferStage>,
    config: &'a Config,
    snapshots: &'a watch::Sender<Snapshot>,
    ledger: &'a mut RunLedger,
    participants: Vec<String>,
    members: Vec<Member>,
    starts: FuturesUnordered<Start<'a>>,
    latency: JoinSet<LatencyCompletion>,
    events: SelectAll<BoxStream<'static, (String, Observation)>>,
    hosts: BTreeMap<String, HostLatency>,
    retired: JoinSet<()>,
    removed: Vec<String>,
    lost: Option<ServerError>,
    window: Option<(Instant, Instant)>,
}

pub(super) async fn measure(
    stage: Stage,
    config: &Config,
    servers: &[PreparedServer],
    snapshots: &watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
    ledger: &mut RunLedger,
) -> Result<Vec<String>, Error> {
    let mut run = StageRun::open(stage, config, servers, snapshots, ledger)?;
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
        ledger: &'a mut RunLedger,
    ) -> Result<Self, Error> {
        let operation_limit = planned_warmup(config, servers.iter())
            .checked_add(config.duration(stage))
            .and_then(|duration| duration.checked_add(Duration::from_secs(60)))
            .ok_or("stage duration overflow")?;
        let ids = servers.iter().map(|server| server.entry.id.clone());
        snapshots.send_modify(|snapshot| snapshot.open_stage(stage, ids));
        let opened = Instant::now();
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
            ledger,
            participants: servers.iter().map(|server| server.entry.id.clone()).collect(),
            members: Vec::new(),
            starts: FuturesUnordered::new(),
            latency: JoinSet::new(),
            events: SelectAll::new(),
            hosts: BTreeMap::new(),
            retired: JoinSet::new(),
            removed: Vec::new(),
            lost: None,
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
                moved: [opened; 2],
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
        // A session settles at most its window of probes at once, 2, 4 or 16; the rest is headroom
        // for observations queued during receiver checkpoints.
        let (observations, receiver) = mpsc::channel(1024);
        self.events.push(
            futures_util::stream::unfold((id.clone(), receiver), |(id, mut receiver)| async {
                receiver.recv().await.map(|event| ((id.clone(), event), (id, receiver)))
            })
            .boxed(),
        );
        self.hosts.insert(id.clone(), HostLatency::default());
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
                (interval, operation_limit, window),
                observations,
                stopped.clone(),
            )
            .await;
            // A session ends Ok past Running only once the stage stopped it or its window ended,
            // which ends it as Go's probes.ended does (latency.go:252-253).
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
    /// Started lanes are checked as in warmup, so a loss is noticed while others still start.
    async fn ready(&mut self) -> Result<(), Error> {
        let ready_by = Instant::now() + STAGE_READY_TIMEOUT;
        let mut expired = false;
        let mut health = tokio::time::interval(SAMPLE_INTERVAL);
        while !self.starts.is_empty() || self.members.iter().any(Member::dialling) {
            tokio::select! {
                Some((id, started)) = self.starts.next(), if !self.starts.is_empty() => self.started(&id, started)?,
                Some(event) = self.events.next(), if !self.events.is_empty() => self.observe_latency(event),
                Some(joined) = self.latency.join_next(), if !self.latency.is_empty() => self.latency_ended(joined)?,
                _ = health.tick() => self.check_health()?,
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
                        removed |= self.fail(&id, scope, error, ready_by);
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
                let removed = self.fail(id, FailureScope::Throughput, error, Instant::now());
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
        let members = servers
            .iter()
            .filter(|server| self.members.iter().any(|member| member.id == server.entry.id));
        let warmup = planned_warmup(self.config, members);
        self.snapshots.send_modify(|snapshot| snapshot.phase = Phase::Warmup);
        let end = Instant::now() + warmup;
        // Lanes are checked as often as in the window, so a loss is noticed when it happens, as
        // Go's stage handles each outcome as it arrives (stage.go:240-260).
        let mut health = tokio::time::interval(SAMPLE_INTERVAL);
        loop {
            self.check_health()?;
            tokio::select! {
                () = tokio::time::sleep_until(end) => return Ok(()),
                _ = health.tick() => {}
                Some(event) = self.events.next(), if !self.events.is_empty() => self.observe_latency(event),
                Some(joined) = self.latency.join_next(), if !self.latency.is_empty() => self.latency_ended(joined)?,
            }
        }
    }

    async fn open_window(&mut self) -> Result<(), Error> {
        let initial = match self.transfer {
            Some(_) => {
                let (initial, misses) = self.collect(CHECKPOINT_BUDGET, None).await.expect("no stage end");
                // Go records any miss here as the preparation failing, whatever the checkpoint's cause.
                let unprepared = misses
                    .into_keys()
                    .map(|id| (id, "receiver checkpoint unavailable before measurement".into()))
                    .collect();
                self.depart(unprepared)?;
                self.check_health()?;
                Some(initial)
            }
            None => None,
        };
        let started = Instant::now();
        let end = started + self.config.duration(self.stage);
        self.window = Some((started, end));
        if let (Some(stage), Some(mut initial)) = (self.transfer, initial) {
            // Download counters restart at the actual measurement start; as in Go, the observed upload
            // stays as read before the checkpoints, so what the receiver took meanwhile counts.
            let local = self.local_boundary();
            initial.at_nanos = local.at_nanos;
            initial.down = local.down;
            let participants = self.members.iter().map(|member| member.id.clone()).collect();
            let accounting = &mut self.ledger.accounting;
            accounting.begin_stage(stage, participants, initial.at_nanos);
            accounting.observe(initial);
        }
        for member in &mut self.members {
            member.moved = [started; 2];
            // Go's probes.open (latency.go:233): the window's end bounds a lost channel's redial.
            member.stop_latency(Stop::Window(end));
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
        self.depart(retrying)
    }

    fn check_health(&mut self) -> Result<(), Error> {
        let failures = self
            .members
            .iter_mut()
            .filter_map(|member| member.health().err().map(|error| (member.id.clone(), error)))
            .collect();
        self.depart(failures)
    }

    fn depart(&mut self, failures: Vec<(String, Error)>) -> Result<(), Error> {
        let mut removed = false;
        let at = Instant::now();
        for (id, error) in failures {
            removed |= self.fail(&id, FailureScope::Throughput, error, at);
        }
        self.settle(removed)
    }

    /// Records a failure once, at its time on the run's clock, with one reason, Go's for a failure
    /// before the window opened or in it (stage.go:195-197); a throughput failure, or a
    /// latency-stage loss beside another server, removes it.
    fn fail(&mut self, id: &str, scope: FailureScope, error: Error, at: Instant) -> bool {
        let Some(index) = self.members.iter().position(|member| member.id == id) else {
            return false;
        };
        let at = self.ledger.since_start(at);
        let reason = crate::failure::reason(error.as_ref(), self.window.is_none());
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
            snapshot.failure(id, scope, reason, at);
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
    /// As in Go, only a transfer's open window touches the run's account, whose last interval is
    /// otherwise an earlier stage's.
    fn settle(&mut self, removed: bool) -> Result<(), Error> {
        if removed && self.transfer.is_some() && self.window.is_some() {
            let survivors: Vec<_> = self.members.iter().map(|member| member.id.clone()).collect();
            let at = nanos(self.ledger.started.elapsed());
            self.ledger.accounting.dropout(&survivors, at);
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
        let removed = self.fail(&completion.id, FailureScope::Latency, error, completion.at);
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
            at_nanos: nanos(self.ledger.started.elapsed()),
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

    /// Shared silence belongs to the link; a quiet member leaves while another moves, or at the stage end.
    fn observe_boundary(
        &mut self,
        boundary: Boundary,
        mut misses: BTreeMap<String, Error>,
    ) -> Result<Option<AggregateWindow>, Error> {
        let final_boundary = boundary.final_boundary;
        let collected = self.ledger.started + Duration::from_nanos(boundary.at_nanos);
        let directions: &[Direction] = match self.transfer {
            Some(TransferStage::Download) => &[Direction::Down],
            Some(TransferStage::Upload) => &[Direction::Up],
            _ => &[Direction::Down, Direction::Up],
        };
        let accounting = &mut self.ledger.accounting;
        let before: Vec<[u64; 2]> = self
            .members
            .iter()
            .map(|member| [Direction::Down, Direction::Up].map(|direction| accounting.bytes(&member.id, direction)))
            .collect();
        let window = accounting.observe(boundary);
        for (member, before) in self.members.iter_mut().zip(before) {
            for direction in directions {
                if accounting.bytes(&member.id, *direction) > before[*direction as usize] {
                    member.moved[*direction as usize] = collected;
                }
            }
        }
        let mut departures = Vec::new();
        for index in 0..self.members.len() {
            let moving = |direction: Direction| {
                self.members.iter().enumerate().any(|(other, member)| {
                    other != index
                        && collected.saturating_duration_since(member.moved[direction as usize]) < STALL_QUIET
                })
            };
            let up_moving = moving(Direction::Up);
            let stalled = directions.iter().any(|direction| {
                collected.saturating_duration_since(self.members[index].moved[*direction as usize]) >= REDIAL_WINDOW
                    && (final_boundary || moving(*direction))
            });
            let member = &mut self.members[index];
            if let Some(error) = member.missed(misses.remove(&member.id), final_boundary, up_moving) {
                departures.push((member.id.clone(), error));
            } else if stalled {
                let stalled: Error = Box::new(Failure::Measurement(FailureReason::Timeout));
                departures.push((member.id.clone(), stalled));
            }
        }
        self.depart(departures)?;
        Ok(window)
    }

    fn publish(&mut self, started: Instant, window: Option<&AggregateWindow>) {
        let elapsed = started.elapsed();
        let hosts = &mut self.hosts;
        self.snapshots.send_modify(|snapshot| {
            sample_hosts(hosts, snapshot);
            snapshot.latest = Point {
                elapsed,
                sample_count: 1,
                down_bps: window
                    .and_then(|window| window.down_bytes_per_sec)
                    .map(|rate| rate * 8.0),
                up_bps: window.and_then(|window| window.up_bytes_per_sec).map(|rate| rate * 8.0),
            };
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
        let now = Instant::now();
        let (started, end) = self.window.unwrap_or((now, now));
        let ended = end.min(now);
        let accounting = &self.ledger.accounting;
        let measured = self.transfer.filter(|_| measuring);
        let mut result = StageResult {
            stage: self.stage,
            elapsed: ended.saturating_duration_since(started),
            down: measured
                .filter(|stage| stage.needs_down())
                .map(|_| accounting.result(Direction::Down)),
            up: measured
                .filter(|stage| stage.needs_up())
                .map(|_| accounting.result(Direction::Up)),
            stopped,
            ..StageResult::default()
        };
        let missing = result.lacks_throughput();
        // The run's account holds earlier stages; a stage that never opened its window has no results there.
        let own = |id: &str, direction| {
            if measuring {
                accounting.server_result(id, direction)
            } else {
                MeasurementResult::unavailable(direction, 0)
            }
        };
        let at = self.ledger.since_start(now);
        let hosts = &mut self.hosts;
        let (transfer, members, participants) = (self.transfer, &self.members, &self.participants);
        self.snapshots.send_modify(|snapshot| {
            if measuring {
                sample_hosts(hosts, snapshot);
            }
            // Only a server whose latency this stage measured has a population, as in Go.
            result.server_latencies = snapshot
                .server_latencies
                .iter()
                .filter_map(|host| Some((host, hosts.get(&host.id)?)))
                .map(|(host, own)| ServerLatencyResult {
                    elapsed: own
                        .ended_at
                        .filter(|_| measuring)
                        .map(|at| at.min(ended).saturating_duration_since(started)),
                    id: host.id.clone(),
                    summary: own.accumulator.snapshot(),
                    ending: own.ending.or(stopped.then_some(Ending::Stopped)),
                })
                .collect();
            // As Go's close(), a stopped stage also names the evidence it lacked.
            if measuring {
                let insufficient = FailureReason::InsufficientEvidence;
                let throughput_failed = snapshot
                    .failures
                    .iter()
                    .any(|failure| failure.stage == result.stage && failure.scope == FailureScope::Throughput);
                for member in members {
                    if missing && !throughput_failed {
                        snapshot.failure(&member.id, FailureScope::Throughput, insufficient, at);
                    }
                    let unmeasured = result
                        .server_latencies
                        .iter()
                        .any(|host| host.id == member.id && host.median().is_none());
                    if result.stage == Stage::Latency && unmeasured {
                        snapshot.failure(&member.id, FailureScope::Latency, insufficient, at);
                    }
                }
            }
            result.server_results = transfer
                .map(|transfer| {
                    participants
                        .iter()
                        .map(|id| ServerContribution {
                            id: id.clone(),
                            down: transfer.needs_down().then(|| own(id, Direction::Down)),
                            up: transfer.needs_up().then(|| own(id, Direction::Up)),
                        })
                        .collect()
                })
                .unwrap_or_default();
            snapshot.results.push(result);
            snapshot.intervals.clone_from(accounting.intervals());
            snapshot.omitted_intervals = accounting.omitted_intervals();
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
    let fetch = target.transport == ThroughputTransport::FetchStream;
    let (down, up) = config.lanes(target);
    let upload_transport = if stage == Stage::Bidirectional && fetch && target.protocol == Protocol::Http3 {
        // Sustained downloads can occupy the connection send window
        // and starve upload control traffic at high lane counts.
        let connect = Transport::connect(server.client.clone(), &target.base_url, target.protocol);
        tokio::select! {
            biased;
            _ = stopped.wait_for(|stopped| *stopped) => return Err("stage stopped before its lanes started".into()),
            connection = connect => Arc::new(connection?),
        }
    } else {
        transport.clone()
    };
    // Both directions start together, as Go's roles do.
    let download = OptionFuture::from(stage.downloads().then_some(async {
        if fetch {
            let stagger = lane_stagger(config.warmup, server.idle_rtt, down);
            Download::start(transport.clone(), down, operation_limit, stagger, stopped.clone()).await
        } else {
            Download::start_webtransport(&server.client, target, down, operation_limit, stopped.clone()).await
        }
    }));
    let upload = OptionFuture::from(stage.uploads().then_some(async {
        let (replaced, stopped) = (server.replaced_upload.clone(), stopped.clone());
        let http = fetch.then(|| (lane_stagger(config.warmup, server.idle_rtt, up), operation_limit));
        Upload::start(upload_transport, up, http, replaced, stopped).await
    }));
    let (down, up) = tokio::join!(download, upload);
    let (down, up, error) = match (down.transpose(), up.transpose()) {
        (Ok(down), Ok(up)) => (down, up, None),
        (Err(error), up) => (None, up.ok().flatten(), Some(error)),
        (down, Err(error)) => (down.ok().flatten(), None, Some(error)),
    };
    let lanes = Lanes {
        down,
        up,
        ..Lanes::default()
    };
    let Some(error) = error else {
        return Ok(lanes);
    };
    // A bidirectional member may have one live direction when the other cannot start.
    let _ = lanes.close(false).await;
    Err(error)
}

fn observe(
    hosts: &mut BTreeMap<String, HostLatency>,
    window: Option<(Instant, Instant)>,
    (id, event): (String, Observation),
) {
    if let (Some(host), Some((start, end))) = (hosts.get_mut(&id), window) {
        // Go's chart takes every reply and timeout of a probe the window measures.
        let charted = match event {
            Observation::Sample { sent, rtt, .. } => {
                host.timeouts = 0;
                Some((sent, sent + rtt, rtt.as_secs_f64() * 1000.0))
            }
            Observation::Lost {
                sent,
                outcome: ProbeOutcome::Timeout,
            } => {
                host.timeouts += 1;
                Some((sent, Instant::now(), f64::NAN))
            }
            Observation::Lost { .. } | Observation::ConnectionBoundary => None,
        };
        if let Some((_, at, ms)) = charted.filter(|(sent, ..)| (start..end).contains(sent)) {
            crate::model::ServerLatency::step(&mut host.steps, at, ms);
        }
        observe_latency(event, start, end, &mut host.accumulator, &mut host.latest);
    }
}

fn sample_hosts(hosts: &mut BTreeMap<String, HostLatency>, snapshot: &mut Snapshot) {
    for host in &mut snapshot.server_latencies {
        host.timeouts = hosts.get(&host.id).map_or(0, |state| state.timeouts);
        host.latest_ms = hosts.get_mut(&host.id).and_then(|state| state.latest.take());
        host.steps = hosts
            .get_mut(&host.id)
            .map(|state| std::mem::take(&mut state.steps))
            .unwrap_or_default();
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

/// The longest adaptive warmup of `servers`, at least the configured one.
fn planned_warmup<'a>(config: &Config, servers: impl Iterator<Item = &'a PreparedServer>) -> Duration {
    servers.fold(config.warmup, |warmup, server| {
        warmup.max(adaptive_warmup(config.warmup, server.idle_rtt))
    })
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
