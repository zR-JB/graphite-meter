//! A run: stages over the prepared participants and engine, membership between stages, idle-round-trip warmups, events.
use super::{
    engine::{
        Decision, Engine, Input, Member, ProberEvent, Sample, StagePlan, Tick, lateness, stagger,
        warmup,
    },
    prepare::{Prepared, ServerPath},
    probe::Prober,
    select,
    upload::UploadSession,
};
use crate::{
    config::Config,
    events::{Event, Events, Probed},
    measure::{
        aggregate::{Reading, Receiver},
        latency::ProbeOutcome,
    },
    model::{
        Dir, Direction, Failure, LaneHealth, Outcome, Scope, ServerFailure, Stage, StageResult,
    },
    net::{Client, Lanes, Work, topology},
};
use futures_util::future::join_all;
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::{task::JoinHandle, time::sleep_until};
use tokio_util::sync::{CancellationToken, DropGuard};

const NO_SURVIVORS: &str = "All selected test servers failed";

struct Seat<'a> {
    server: &'a ServerPath,
    idle_rtt: Option<Duration>,
    /// Its one replacement upload receiver in the run.
    replaced: Arc<AtomicBool>,
    present: bool,
}

struct Run<'a> {
    prepared: &'a Prepared,
    config: &'a Config,
    events: &'a Events,
    token: &'a CancellationToken,
}

/// Runs `config`'s stages over the servers `prepared` has paths for, until done or `token` is cancelled.
pub async fn run(
    prepared: &Prepared,
    config: &Config,
    events: &Events,
    token: CancellationToken,
) -> Outcome {
    let started = Instant::now();
    let unprepared = prepared.servers.iter().filter_map(|server| {
        let (failure, scope) = (server.path.as_ref().err()?.clone(), Scope::Server);
        Some(ServerFailure {
            server: server.id.clone(),
            scope,
            failure,
            at: started,
        })
    });
    let unprepared: Vec<_> = unprepared.collect();
    let mut seats: Vec<_> = prepared.servers.iter().filter_map(Seat::new).collect();
    let plan = config.plan();
    if let Some(error) = refusal(&prepared.servers, &unprepared, &plan) {
        let (outcome, elapsed) = (Outcome::Failed, started.elapsed());
        events.send(Event::RunFinished {
            outcome,
            error: Some(error),
            elapsed,
        });
        return outcome;
    }
    let focus = seats[0].server.id.clone();
    events.send(Event::RunStarted {
        plan: plan.clone(),
        focus,
        at: started,
        loaded_latency: config.loaded_latency(),
    });
    let run = Run {
        prepared,
        config,
        events,
        token: &token,
    };
    unprepared.iter().for_each(|failure| run.failed(failure));
    let (sole, mut results, mut error) = (prepared.servers.len() == 1, Vec::new(), None);
    for (stage, duration) in &plan {
        if token.is_cancelled() {
            break;
        }
        let members: Vec<_> = seats.iter().filter(|seat| seat.present).collect();
        let mut result = run.stage(*stage, *duration, &members).await;
        if results.is_empty() {
            result.failures.splice(0..0, unprepared.iter().cloned());
        }
        for server in &result.servers {
            let Some(seat) = seats
                .iter_mut()
                .find(|seat| seat.server.id == server.server)
            else {
                continue;
            };
            seat.present &= !server.left;
            let median = server.latency.and_then(|latency| latency.summary.p50);
            seat.idle_rtt = median
                .filter(|_| *stage == Stage::Latency)
                .or(seat.idle_rtt);
        }
        events.send(Event::StageFinished(result.clone()));
        results.push(result);
        if !token.is_cancelled() && seats.iter().all(|seat| !seat.present) {
            match survivor(&results, &prepared.servers, sole) {
                Ok(()) => seats[0].present = true,
                Err(failure) => {
                    error = Some(failure);
                    break;
                }
            }
        }
    }
    let outcome = match token.is_cancelled() && results.len() < plan.len() {
        true => Outcome::Stopped,
        false => Outcome::of(&results, &config.stages),
    };
    events.send(Event::RunFinished {
        outcome,
        error,
        elapsed: started.elapsed(),
    });
    outcome
}

/// Whether a sole departed server stays for the next stage: once measured, unless it needs sign-in; else why it ends.
fn survivor(results: &[StageResult], servers: &[ServerPath], sole: bool) -> Result<(), Failure> {
    let mut failures = results.iter().flat_map(|result| &result.failures);
    let last =
        failures.rfind(|failure| failure.failure.reason != FailureReason::InsufficientEvidence);
    let measured = results.iter().any(|result| !result.measured.is_zero());
    let name = |id: &ServerId| {
        servers
            .iter()
            .find(|server| server.id == *id)
            .map_or("", |server| server.name.as_str())
    };
    match last {
        _ if sole
            && measured
            && last.is_none_or(|last| last.failure.reason != FailureReason::SignInRequired) =>
        {
            Ok(())
        }
        Some(last) => {
            let text = format!(
                "{NO_SURVIVORS}. {}: {}",
                name(&last.server),
                last.failure.text
            );
            Err(Failure::new(last.failure.reason, text))
        }
        None => Err(Failure::new(
            FailureReason::ConnectionLost,
            format!("{NO_SURVIVORS}."),
        )),
    }
}

/// Why the run cannot start: no server has a path, or a planned stage outlasts a server's limit.
fn refusal(
    servers: &[ServerPath],
    unprepared: &[ServerFailure],
    plan: &[(Stage, Duration)],
) -> Option<Failure> {
    let refused = |text: String| Failure::new(FailureReason::PathFailed, text);
    match unprepared.first() {
        _ if servers.iter().any(|server| server.path.is_ok()) => {
            select::fit(plan, servers).err().map(refused)
        }
        Some(first) => Some(first.failure.clone()),
        None => Some(refused("no test server was prepared".into())),
    }
}

impl<'a> Seat<'a> {
    fn new(server: &'a ServerPath) -> Option<Self> {
        let idle_rtt = server.path.as_ref().ok()?.idle_rtt;
        Some(Self {
            server,
            idle_rtt,
            replaced: Arc::default(),
            present: true,
        })
    }
}

impl Run<'_> {
    /// `stage` over `seats` for `duration`; a stop in its window finishes it as stopped.
    async fn stage(&self, stage: Stage, duration: Duration, seats: &[&Seat<'_>]) -> StageResult {
        let config = self.config;
        let members = seats.iter().map(|seat| Member {
            server: seat.server.id.clone(),
            warmup: warmup(config.warmup, seat.idle_rtt),
        });
        let latency = match stage {
            Stage::Latency => Some(config.ping),
            _ => config.loaded_latency().then_some(config.loaded_ping),
        };
        let plan = StagePlan {
            stage,
            members: members.collect(),
            duration,
            latency,
        };
        self.events.send(Event::StageStarted(plan.clone()));
        let mut live = Live {
            run: self,
            stage,
            engine: Engine::new(plan.clone(), Instant::now()),
            participants: Vec::new(),
            departed: Vec::new(),
            checkpoint: None,
            window: None,
            open: false,
            finished: false,
        };
        let token = self.token.child_token();
        let opening = seats.iter().map(|seat| {
            let replaced = seat.replaced.clone();
            Participant::open(
                &self.prepared.client,
                seat.server,
                &plan,
                config,
                replaced,
                token.child_token(),
            )
        });
        match self.token.run_until_cancelled(join_all(opening)).await {
            Some(opened) => {
                for (seat, opened) in seats.iter().zip(opened) {
                    match opened {
                        Ok(participant) => live.participants.push(participant),
                        Err(failure) => live.departed.push((seat.server.id.clone(), failure)),
                    }
                }
                live.run().await;
            }
            None => live.engine.stop(Instant::now()),
        }
        join_all(live.participants.into_iter().map(Participant::finish)).await;
        live.engine.result()
    }

    /// Announces a failure, except a missing result's, which the stage's result carries.
    fn failed(&self, failure: &ServerFailure) {
        if failure.failure.reason != FailureReason::InsufficientEvidence {
            self.events.send(Event::ServerFailed(failure.clone()));
        }
    }
}

/// A stage in progress: its engine, the participants still in it, and what the engine asked for.
struct Live<'a> {
    run: &'a Run<'a>,
    stage: Stage,
    engine: Engine,
    participants: Vec<Participant>,
    /// Participants that could not open, for the next tick.
    departed: Vec<(ServerId, Failure)>,
    checkpoint: Option<Duration>,
    /// The measured window once it opened; probes sent within it are announced until the stage ends.
    window: Option<(Instant, Instant)>,
    open: bool,
    finished: bool,
}

impl Live<'_> {
    async fn run(&mut self) {
        let token = self.run.token;
        let (mut due, mut returned) = (Instant::now(), Instant::now());
        while !self.finished {
            if token
                .run_until_cancelled(sleep_until(due.into()))
                .await
                .is_none()
            {
                return self.engine.stop(Instant::now());
            }
            let fired = Instant::now();
            let ticked =
                token.run_until_cancelled(self.tick(fired, lateness(fired, due, returned)));
            let Some((tick, probes)) = ticked.await else {
                return self.engine.stop(Instant::now());
            };
            due = tick.next;
            self.apply(tick, &probes, fired);
            returned = Instant::now();
        }
    }

    /// Snapshots every participant, gathers the checkpoints asked for, drains probes and ticks the engine.
    async fn tick(
        &mut self,
        now: Instant,
        lateness: Duration,
    ) -> (Tick, Vec<(ServerId, ProberEvent)>) {
        let mut samples: Vec<_> = self
            .participants
            .iter_mut()
            .map(Participant::local)
            .collect();
        if let Some(budget) = self.checkpoint.take() {
            let gathered = self
                .participants
                .iter()
                .map(|member| member.checkpoint(budget));
            for (sample, checkpoint) in samples.iter_mut().zip(join_all(gathered).await) {
                match checkpoint {
                    Some(Ok(receiver)) => sample.reading.up = Some(receiver),
                    Some(Err(failure)) => sample.missed = Some(failure),
                    None => {}
                }
            }
        }
        let mut probes = Vec::new();
        for participant in &mut self.participants {
            participant.probes(&mut probes);
        }
        let departed = std::mem::take(&mut self.departed);
        let input = Input {
            now,
            lateness,
            samples: &samples,
            probes: &probes,
            departed: &departed,
        };
        (self.engine.tick(input), probes)
    }

    fn apply(&mut self, mut tick: Tick, probes: &[(ServerId, ProberEvent)], now: Instant) {
        let (participants, mut closed) = (&mut self.participants, false);
        for decision in std::mem::take(&mut tick.decisions) {
            match decision {
                Decision::OpenWindow { start, end } => {
                    (self.window, self.open) = (Some((start, end)), true);
                    participants
                        .iter()
                        .for_each(|participant| participant.opened(end));
                    self.run.events.send(Event::Measuring(self.stage));
                }
                Decision::Checkpoint(budget) => self.checkpoint = Some(budget),
                Decision::CloseWindow => {
                    closed = true;
                    participants.iter_mut().for_each(Participant::close);
                }
                Decision::Failed(failure) => {
                    let index = participants
                        .iter()
                        .position(|open| open.server == failure.server);
                    if let Some(index) = index.filter(|_| failure.scope == Scope::Latency) {
                        participants[index].stop_probing();
                    }
                    self.run.failed(&failure);
                }
                Decision::Remove(failure) => {
                    let index = participants
                        .iter()
                        .position(|open| open.server == failure.server);
                    if let Some(index) = index {
                        participants.remove(index).depart();
                    }
                    self.run.failed(&failure);
                }
                Decision::Finish => self.finished = true,
            }
        }
        if let Some(window) = self.window {
            self.announce(&tick, probes, now, window);
        }
        self.open &= !closed;
    }

    /// Announces each server's probes sent within the window and, while it is open, the live rates `now` into it.
    fn announce(
        &self,
        tick: &Tick,
        probes: &[(ServerId, ProberEvent)],
        now: Instant,
        (start, end): (Instant, Instant),
    ) {
        let events = self.run.events;
        // Each participant's probes arrive together.
        for own in probes.chunk_by(|(one, _), (other, _)| one == other) {
            let probed = own.iter().filter_map(|(_, probe)| {
                let (sent, rtt) = match probe {
                    ProberEvent::Outcome {
                        sent,
                        outcome: ProbeOutcome::Reply { rtt, .. },
                    } => (*sent, Some(*rtt)),
                    ProberEvent::Outcome {
                        sent,
                        outcome: ProbeOutcome::Timeout,
                    } => (*sent, None),
                    _ => return None,
                };
                (start..end).contains(&sent).then(|| Probed {
                    at: sent - start,
                    rtt,
                })
            });
            let probes: Box<[Probed]> = probed.collect();
            let server = &own[0].0;
            if let Some(latency) = self.engine.latency(server).filter(|_| !probes.is_empty()) {
                events.send(Event::Probes {
                    server: server.clone(),
                    probes,
                    latency,
                });
            }
        }
        if let Some(rates) = tick.live.filter(|_| self.open) {
            events.send(Event::Sample {
                at: now.saturating_duration_since(start),
                rates,
            });
        }
    }
}

/// One server's share of a stage: its download lanes, upload session and prober; dropping it ends them.
struct Participant {
    server: ServerId,
    down: Option<Lanes>,
    up: Option<UploadSession>,
    prober: Option<Prober>,
    finishing: Option<JoinHandle<()>>,
    _departs: DropGuard,
}

impl Participant {
    /// Starts `server`'s work for `plan` under `token`; `replaced` is its upload replacement for the run.
    async fn open(
        client: &Client,
        server: &ServerPath,
        plan: &StagePlan,
        config: &Config,
        replaced: Arc<AtomicBool>,
        token: CancellationToken,
    ) -> Result<Self, Failure> {
        let departs = token.clone().drop_guard();
        let paths = server.path.as_ref().map_err(Clone::clone)?;
        let member = plan
            .members
            .iter()
            .find(|member| member.server == server.id);
        let warmup = member.map_or(Duration::ZERO, |member| member.warmup);
        let path = &paths.throughput;
        let lanes = config.lanes(path.protocol, path.transport);
        let plans = topology(path, plan.stage, lanes);
        let spacing = |direction| stagger(warmup, lanes[direction]);
        let probed = plan.latency.zip(paths.latency.clone());
        let prober = probed.map(|(cadence, path)| {
            Prober::spawn(
                client.clone(),
                path,
                plan.stage,
                cadence,
                token.child_token(),
            )
        });
        let down = plan.stage.moves(Direction::Down).then(|| {
            Lanes::start(
                client,
                plans.clone(),
                Work::Download,
                spacing(Direction::Down),
                token.child_token(),
            )
        });
        let up = match plan.stage.moves(Direction::Up) {
            true => {
                let child = token.child_token();
                let session = UploadSession::open(
                    client,
                    &server.origin,
                    path,
                    plans,
                    spacing(Direction::Up),
                    replaced,
                    child,
                );
                Some(session.await.map_err(|fault| fault.failure())?)
            }
            false => None,
        };
        Ok(Self {
            server: server.id.clone(),
            down,
            up,
            prober,
            finishing: None,
            _departs: departs,
        })
    }

    /// Its local counters and lane health now; checkpoints come apart.
    fn local(&mut self) -> Sample {
        let reading = Reading {
            server: self.server.clone(),
            down: self.down.as_ref().map(Lanes::bytes),
            up: None,
            fed: self.up.as_ref().and_then(UploadSession::fed),
        };
        let ready = self.down.as_ref().is_none_or(Lanes::ready)
            && self.up.as_ref().is_none_or(UploadSession::ready);
        let lanes = Dir {
            down: self.down.as_mut().map_or(LaneHealth::Ok, Lanes::health),
            up: self
                .up
                .as_mut()
                .map_or(LaneHealth::Ok, UploadSession::health),
        };
        Sample {
            reading,
            ready,
            missed: None,
            lanes,
        }
    }

    async fn checkpoint(&self, budget: Duration) -> Option<Result<Receiver, Failure>> {
        let up = self.up.as_ref()?;
        Some(up.checkpoint(budget).await.map_err(|fault| fault.failure()))
    }

    fn probes(&mut self, into: &mut Vec<(ServerId, ProberEvent)>) {
        if let Some(prober) = &mut self.prober {
            into.extend(prober.drain().map(|probe| (self.server.clone(), probe)));
        }
    }

    fn opened(&self, end: Instant) {
        if let Some(prober) = &self.prober {
            prober.open(end);
        }
    }

    /// The window closed: downloads stop, the upload session finishes and the prober drains.
    fn close(&mut self) {
        self.down = None;
        if let Some(prober) = &self.prober {
            prober.close();
        }
        if let Some(up) = self.up.take() {
            self.finishing = Some(tokio::spawn(up.finish()));
        }
    }

    fn stop_probing(&mut self) {
        self.prober = None;
    }

    /// Leaves the stage: its work ends and its upload receiver is asked to finalize.
    fn depart(mut self) {
        if let Some(up) = self.up.take() {
            up.depart();
        }
    }

    /// Closes it if the window did not, and waits for its upload session to finish.
    async fn finish(mut self) {
        self.close();
        if let Some(finishing) = self.finishing.take() {
            let _ = finishing.await;
        }
    }
}
