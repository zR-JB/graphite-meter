//! A run: stages over the prepared participants and engine, membership between stages, idle-round-trip warmups, events.
use super::{
    engine::{Decision, Engine, Input, Member, Probe, StagePlan, Tick, lateness, warmup},
    participant::Participant,
    prepare::{Prepared, ServerPath},
    select,
};
use crate::{
    config::Config,
    events::{Event, Events},
    measure::latency::ProbeOutcome,
    model::{Failure, Outcome, Scope, ServerFailure, Stage, StageResult},
};
use futures_util::future::join_all;
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::time::sleep_until;
use tokio_util::sync::CancellationToken;

/// How long a stage's upload sessions may take to finish, and a stopped stage's.
const FINISH: Duration = Duration::from_secs(10);
const STOPPED_FINISH: Duration = Duration::from_secs(1);
/// Why a run whose servers all left ends.
const NO_SURVIVORS: &str = "all selected servers failed";

/// A prepared server across the run.
struct Seat<'a> {
    server: &'a ServerPath,
    idle_rtt: Duration,
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
pub async fn run(prepared: &Prepared, config: &Config, events: &Events, token: CancellationToken) -> Outcome {
    let started = Instant::now();
    let unprepared = prepared.servers.iter().filter_map(|server| {
        let (failure, scope) = (server.path.as_ref().err()?.clone(), Scope::Server);
        Some(ServerFailure { server: server.id.clone(), scope, failure, at: started })
    });
    let unprepared: Vec<_> = unprepared.collect();
    let mut seats: Vec<_> = prepared.servers.iter().filter_map(Seat::new).collect();
    let plan = config.plan();
    if let Some(error) = refusal(&prepared.servers, &unprepared, &plan) {
        let (outcome, elapsed) = (Outcome::Failed, started.elapsed());
        events.send(Event::RunFinished { outcome, error: Some(error), elapsed });
        return outcome;
    }
    let focus = seats[0].server.id.clone();
    events.send(Event::RunStarted { plan: plan.clone(), focus, at: started });
    let run = Run { prepared, config, events, token: &token };
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
            let Some(seat) = seats.iter_mut().find(|seat| seat.server.id == server.server) else {
                continue;
            };
            seat.present &= !server.left;
            let median = server.latency.and_then(|latency| latency.summary.p50);
            seat.idle_rtt = median.filter(|_| *stage == Stage::Latency).unwrap_or(seat.idle_rtt);
        }
        events.send(Event::StageFinished(result.clone()));
        results.push(result);
        if !token.is_cancelled() && seats.iter().all(|seat| !seat.present) {
            match survivor(&results, sole) {
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
    events.send(Event::RunFinished { outcome, error, elapsed: started.elapsed() });
    outcome
}

/// Whether a sole departed server stays for the next stage: once measured, unless it needs sign-in; else why it ends.
fn survivor(results: &[StageResult], sole: bool) -> Result<(), Failure> {
    let failures = results.iter().flat_map(|result| &result.failures);
    let last = failures
        .map(|failure| &failure.failure)
        .rfind(|failure| failure.reason != FailureReason::InsufficientEvidence);
    let measured = results.iter().any(|result| !result.measured.is_zero());
    match last {
        _ if sole && measured && last.is_none_or(|last| last.reason != FailureReason::SignInRequired) => Ok(()),
        Some(last) => Err(Failure::new(last.reason, format!("{NO_SURVIVORS}: {}", last.text))),
        None => Err(Failure::new(FailureReason::ConnectionLost, NO_SURVIVORS)),
    }
}

/// Why the run cannot start: no server has a path, or a planned stage outlasts a server's limit.
fn refusal(servers: &[ServerPath], unprepared: &[ServerFailure], plan: &[(Stage, Duration)]) -> Option<Failure> {
    let refused = |text: String| Failure::new(FailureReason::PreparationFailed, text);
    match unprepared.first() {
        _ if servers.iter().any(|server| server.path.is_ok()) => select::fit(plan, servers).err().map(refused),
        Some(first) => Some(first.failure.clone()),
        None => Some(refused("no server was prepared".into())),
    }
}

impl<'a> Seat<'a> {
    fn new(server: &'a ServerPath) -> Option<Self> {
        let idle_rtt = server.path.as_ref().ok()?.idle_rtt;
        Some(Self { server, idle_rtt, replaced: Arc::default(), present: true })
    }
}

impl Run<'_> {
    /// `stage` over `seats` for `duration`; a stop finishes it as stopped.
    async fn stage(&self, stage: Stage, duration: Duration, seats: &[&Seat<'_>]) -> StageResult {
        let config = self.config;
        let members = seats.iter().map(|seat| Member {
            server: seat.server.id.clone(),
            warmup: warmup(config.warmup, seat.idle_rtt),
        });
        let latency = match stage {
            Stage::Latency => Some(config.ping),
            _ => config.loaded_latency.then_some(config.loaded_ping),
        };
        let plan = StagePlan { stage, members: members.collect(), duration, latency };
        self.events.send(Event::StageStarted(plan.clone()));
        let mut live = Live {
            run: self,
            stage,
            engine: Engine::new(plan.clone(), Instant::now()),
            participants: Vec::new(),
            departed: Vec::new(),
            checkpoint: None,
            window: None,
            finished: false,
        };
        let token = self.token.child_token();
        let opening = seats.iter().map(|seat| {
            let replaced = seat.replaced.clone();
            Participant::open(&self.prepared.client, seat.server, &plan, config, replaced, token.child_token())
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
        let budget = if self.token.is_cancelled() { STOPPED_FINISH } else { FINISH };
        join_all(live.participants.into_iter().map(|member| member.finish(budget))).await;
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
    /// The window's start while it is open.
    window: Option<Instant>,
    finished: bool,
}

impl Live<'_> {
    /// Ticks when the engine asks until it finishes or the run stops.
    async fn run(&mut self) {
        let token = self.run.token;
        let (mut due, mut returned) = (Instant::now(), Instant::now());
        while !self.finished {
            if token.run_until_cancelled(sleep_until(due.into())).await.is_none() {
                return self.engine.stop(Instant::now());
            }
            let fired = Instant::now();
            let ticked = token.run_until_cancelled(self.tick(fired, lateness(fired, due, returned)));
            let Some((tick, probes)) = ticked.await else {
                return self.engine.stop(Instant::now());
            };
            due = tick.next;
            self.apply(tick, &probes, fired);
            returned = Instant::now();
        }
    }

    /// Snapshots every participant, gathers the checkpoints asked for, drains probes and ticks the engine.
    async fn tick(&mut self, now: Instant, lateness: Duration) -> (Tick, Vec<(ServerId, Probe)>) {
        let mut samples: Vec<_> = self.participants.iter_mut().map(Participant::local).collect();
        if let Some(budget) = self.checkpoint.take() {
            let gathered = self.participants.iter().map(|member| member.checkpoint(budget));
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

    /// Carries out the tick's decisions, then announces what it measured.
    fn apply(&mut self, mut tick: Tick, probes: &[(ServerId, Probe)], now: Instant) {
        let (participants, mut closed) = (&mut self.participants, false);
        for decision in std::mem::take(&mut tick.decisions) {
            match decision {
                Decision::OpenWindow { start, end } => {
                    self.window = Some(start);
                    participants.iter().for_each(|participant| participant.opened(end));
                    self.run.events.send(Event::Measuring(self.stage));
                }
                Decision::Checkpoint(budget) => self.checkpoint = Some(budget),
                Decision::CloseWindow => {
                    closed = true;
                    participants.iter_mut().for_each(|member| member.close(FINISH));
                }
                Decision::Failed(failure) => {
                    let index = participants.iter().position(|open| open.server == failure.server);
                    if let Some(index) = index.filter(|_| failure.scope == Scope::Latency) {
                        participants[index].stop_probing();
                    }
                    self.run.failed(&failure);
                }
                Decision::Remove(failure) => {
                    let index = participants.iter().position(|open| open.server == failure.server);
                    if let Some(index) = index {
                        participants.remove(index).depart();
                    }
                    self.run.failed(&failure);
                }
                Decision::Finish => self.finished = true,
            }
        }
        if let Some(start) = self.window {
            self.announce(&tick, probes, now.saturating_duration_since(start), start);
        }
        if closed {
            self.window = None;
        }
    }

    /// Announces the window's in-window probes and the boundary's live rates `at` into the window.
    fn announce(&self, tick: &Tick, probes: &[(ServerId, Probe)], at: Duration, start: Instant) {
        let events = self.run.events;
        for (server, probe) in probes {
            let (sent, rtt) = match probe {
                Probe::Outcome { sent, outcome: ProbeOutcome::Reply { rtt, .. } } => (*sent, Some(*rtt)),
                Probe::Outcome { sent, outcome: ProbeOutcome::Timeout } => (*sent, None),
                _ => continue,
            };
            if let Some(at) = sent.checked_duration_since(start) {
                events.send(Event::Probe { server: server.clone(), at, rtt });
            }
        }
        if let Some(rates) = tick.live {
            events.send(Event::Sample { at, rates, recovering: tick.recovering });
        }
    }
}
