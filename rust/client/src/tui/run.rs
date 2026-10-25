//! Reads a run for the console, from stage states and shown rates to the needle and the latency server's populations.
use super::{
    App,
    chrome::Progress,
    console::{LADDER, ceil_step},
};
use crate::{
    events::{Event, Run, Running},
    measure::latency::Summary,
    model::{Dir, Direction, Stage, StageStatus},
    text::Line,
};
use graphite_meter_proto::catalog::ServerId;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

/// The needle closes on its reading by e⁻¹ every 110 ms, so it rises, falls and drains without a jump.
const NEEDLE: f64 = 0.11;

#[derive(Debug, Default)]
pub struct Live {
    since: Option<Instant>,
    pub shown: Dir<Glide>,
    probes: Vec<(ServerId, Option<Duration>, usize)>,
    /// Scales the dial while idle latency is measured, in milliseconds. It only grows during a run.
    pub rtt_scale: f64,
    pub pick: Option<ServerId>,
    /// Records where the needle stands, 0 to 1 of the sweep, and when it last moved.
    needle: Cell<(f64, Option<Instant>)>,
    sweep: Option<(Instant, f64)>,
}

/// A shown rate moves to each new value over the time between samples, as the browser's readout does.
#[derive(Debug, Clone, Copy, Default)]
pub struct Glide {
    from: f64,
    pub to: f64,
    at: Option<Instant>,
    over: Duration,
}

impl Glide {
    pub fn value(&self, now: Instant) -> f64 {
        let Some(at) = self.at.filter(|_| !self.over.is_zero()) else {
            return self.to;
        };
        let share = now.saturating_duration_since(at).as_secs_f64() / self.over.as_secs_f64();
        self.from + (self.to - self.from) * share.min(1.0)
    }

    /// Glides toward `to` from where the value stands at `now`. A first value snaps.
    fn toward(self, to: f64, now: Instant) -> Self {
        let over = self.at.map_or(Duration::ZERO, |at| {
            now.saturating_duration_since(at)
                .clamp(Duration::from_millis(50), Duration::from_millis(400))
        });
        Self {
            from: self.value(now),
            to,
            at: Some(now),
            over,
        }
    }
}

impl Live {
    /// Takes `event` once `run` applied it. `shown` is the latency server.
    pub(super) fn event(
        &mut self,
        event: &Event,
        run: Option<&Run>,
        shown: Option<&ServerId>,
        now: Instant,
    ) {
        match event {
            Event::RunStarted { .. } => *self = Self::default(),
            Event::StageStarted(_) => {
                (self.since, self.shown) = (Some(now), Dir::default());
                self.probes.clear();
            }
            Event::Measuring(_) => self.since = Some(now),
            Event::Sample { rates, .. } => {
                let Some(run) = run else { return };
                for direction in Direction::BOTH {
                    self.shown[direction] =
                        match (rates[direction], run.live[direction].presented()) {
                            (None, _) => Glide::default(),
                            (Some(_), Some(presented)) => {
                                self.shown[direction].toward(presented, now)
                            }
                            (Some(_), None) => self.shown[direction],
                        };
                }
            }
            Event::Probes { server, probes, .. } => {
                let at = self.probes.iter().position(|(probed, ..)| probed == server);
                let at = at.unwrap_or_else(|| {
                    self.probes.push((server.clone(), None, 0));
                    self.probes.len() - 1
                });
                let (_, latest, streak) = &mut self.probes[at];
                for probe in probes {
                    (*latest, *streak) = match probe.rtt {
                        Some(rtt) => (Some(rtt), 0),
                        None => (*latest, *streak + 1),
                    };
                }
                let idle = run
                    .and_then(|run| run.stage.as_ref())
                    .is_some_and(|running| running.plan().stage == Stage::Latency);
                let slowest = probes.iter().filter_map(|probe| probe.rtt).max();
                if let Some(rtt) = slowest.filter(|_| idle && Some(server) == shown) {
                    let ceiling = ceil_step(rtt.as_secs_f64() * 1e3 * 1.25, &LADDER);
                    self.rtt_scale = self.rtt_scale.max(ceiling);
                }
            }
            Event::RunFinished { .. } => self.sweep = Some((now, self.needle.get().0)),
            Event::Checking(_)
            | Event::Prepared { .. }
            | Event::CheckFailed(_)
            | Event::SignIn(_)
            | Event::SignInEnded(_)
            | Event::ServerFailed(_)
            | Event::StageFinished(_) => {}
        }
    }

    pub fn needle(&self, to: f64, now: Instant) -> f64 {
        let to = to.clamp(0.0, 1.0);
        let (at, then) = self.needle.get();
        let shown = match then {
            Some(then) => {
                to + (at - to) * (-now.saturating_duration_since(then).as_secs_f64() / NEEDLE).exp()
            }
            None => to,
        };
        self.needle.set((shown, Some(now)));
        shown
    }

    /// Sweeps a result arc to `to` from where the needle stood over at most 0.9 s, easing out.
    pub fn swept(&self, to: f64, now: Instant) -> f64 {
        let Some((at, from)) = self.sweep else {
            return to;
        };
        let over = (0.9 * (1.0 - from)).max(0.36);
        let share = (now.saturating_duration_since(at).as_secs_f64() / over).min(1.0);
        to.min(from + (1.0 - from) * (1.0 - (1.0 - share).powi(3)))
    }

    pub fn moving(&self, now: Instant) -> bool {
        let sweeping = self
            .sweep
            .is_some_and(|(at, _)| now.saturating_duration_since(at) < Duration::from_secs(1));
        let (needle, _) = self.needle.get();
        sweeping || needle > 1e-3 && self.sweep.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    Warmup,
    Measuring,
    Ended(StageStatus),
}

impl App {
    pub(super) fn running(&self) -> bool {
        self.view.starting
            || self
                .view
                .run
                .as_ref()
                .is_some_and(|run| run.outcome.is_none())
    }

    pub(super) fn stage_state(&self, stage: Stage) -> State {
        let Some(run) = self.view.run.as_ref() else {
            return State::Pending;
        };
        let current = run
            .stage
            .as_ref()
            .filter(|running| running.plan().stage == stage && run.outcome.is_none());
        match (
            current,
            run.results.iter().rfind(|result| result.stage == stage),
        ) {
            (Some(Running::Measuring { .. }), _) => State::Measuring,
            (Some(Running::Warmup(_)), _) => State::Warmup,
            (None, Some(result)) => State::Ended(result.status(run.focus.as_ref())),
            (None, None) => State::Pending,
        }
    }

    /// `l` steps through these servers, those prepared that have not left the run.
    pub(super) fn participants(&self) -> Vec<ServerId> {
        let servers = self
            .view
            .servers
            .iter()
            .filter(|server| self.view.remains(&server.id));
        servers.map(|server| server.id.clone()).collect()
    }

    /// Returns the shown latency server, the one `l` picked while it remains, else the run's.
    pub(super) fn latency_server(&self) -> Option<&ServerId> {
        let focus = self.view.run.as_ref()?.focus.as_ref();
        let remains = |pick: &&ServerId| {
            self.view.servers.iter().any(|server| server.id == **pick) && self.view.remains(pick)
        };
        self.live.pick.as_ref().filter(remains).or(focus)
    }

    pub(super) fn next_latency_server(&mut self) {
        let servers = self.participants();
        let at = servers
            .iter()
            .position(|id| Some(id) == self.latency_server());
        let next = servers
            .get(at.map_or(0, |at| (at + 1) % servers.len().max(1)))
            .cloned();
        let focus = self.view.run.as_ref().and_then(|run| run.focus.as_ref());
        self.live.pick = next.filter(|next| Some(next) != focus);
    }

    pub(super) fn several(&self) -> bool {
        self.view.servers.len() > 1
    }

    pub(super) fn name(&self, id: Option<&ServerId>) -> String {
        let server = self
            .view
            .servers
            .iter()
            .find(|server| Some(&server.id) == id);
        match (server, id) {
            (Some(server), _) => server.name.clone(),
            (None, Some(id)) => id.as_str().to_owned(),
            (None, None) => crate::format::MISSING.into(),
        }
    }

    pub(super) fn elapsed(&self) -> Duration {
        let since = self.live.since.unwrap_or(self.now);
        self.now.saturating_duration_since(since)
    }

    pub(super) fn reached(&self, stage: Stage) -> f64 {
        let run = self.view.run.as_ref();
        let planned = run
            .and_then(|run| run.plan.iter().find(|(own, _)| *own == stage))
            .map(|(_, duration)| *duration);
        let result = run.and_then(|run| run.results.iter().rfind(|result| result.stage == stage));
        match (planned, result) {
            (Some(planned), Some(result)) => {
                result.measured.as_secs_f64() / planned.as_secs_f64().max(1e-9)
            }
            _ => 0.0,
        }
    }

    /// Gives the latency server's statistics in `stage`, the stage's own once it ended, else its latency so far.
    pub(super) fn population(&self, stage: Stage) -> Option<Summary> {
        let run = self.view.run.as_ref()?;
        let server = self.latency_server()?;
        if let Some(result) = run.results.iter().rfind(|result| result.stage == stage) {
            let population = result
                .servers
                .iter()
                .find(|own| &own.server == server)?
                .latency?;
            return Some(Summary {
                p50: population.median(),
                ..population.summary
            });
        }
        let live = run
            .latency
            .iter()
            .find(|(own, id, _)| *own == stage && id == server);
        live.map(|&(.., summary)| summary)
    }

    pub(super) fn latest(&self) -> Option<Duration> {
        let shown = self.latency_server();
        let probe = self.live.probes.iter().find(|(id, ..)| Some(id) == shown);
        probe.and_then(|(_, rtt, _)| *rtt)
    }

    /// Counts timeouts in a row, in the warning tone and from three on the error tone.
    pub(super) fn timeout_streak(&self) -> Line {
        let shown = self.latency_server();
        match self.live.probes.iter().find(|(id, ..)| Some(id) == shown) {
            Some(&(.., streak @ 1..)) => {
                let style = if streak >= 3 {
                    self.palette.err
                } else {
                    self.palette.warn
                };
                let noun = if streak == 1 { "timeout" } else { "timeouts" };
                Line::styled(format!("{streak} {noun} in a row"), style)
            }
            _ => Line::default(),
        }
    }

    /// Places the stage's measured window on the plan's clock in seconds, once it opened.
    pub(super) fn window(&self, run: &Run, stage: Stage) -> Option<(f64, f64)> {
        let at = run.plan.iter().position(|(planned, _)| *planned == stage)?;
        let offset: Duration = run.plan[..at].iter().map(|(_, duration)| *duration).sum();
        let open =
            matches!(&run.stage, Some(Running::Measuring { plan, .. }) if plan.stage == stage);
        let result = run.results.iter().rfind(|result| result.stage == stage);
        let opened = open || result.is_some_and(|result| !result.measured.is_zero());
        opened.then(|| {
            (
                offset.as_secs_f64(),
                (offset + run.plan[at].1).as_secs_f64(),
            )
        })
    }

    /// The progress bar is busy while preparing, then the planned stage time done. An incomplete stage adds nothing.
    pub(super) fn progress(&self) -> Progress {
        if self.view.starting {
            return Progress::Busy;
        }
        let running = self.view.run.as_ref().filter(|_| self.running());
        let Some(run) = running else {
            return Progress::None;
        };
        let done = |&(stage, duration): &(Stage, Duration)| match self.stage_state(stage) {
            State::Ended(StageStatus::Complete) => duration,
            State::Measuring => self.elapsed().min(duration),
            _ => Duration::ZERO,
        };
        let total: Duration = run.plan.iter().map(|(_, duration)| *duration).sum();
        let done: Duration = run.plan.iter().map(done).sum();
        Progress::Share((done.as_secs_f64() / total.as_secs_f64().max(1.0) * 100.0) as u8)
    }

    pub(super) fn left(&self) -> Option<Duration> {
        let run = self.view.run.as_ref().filter(|_| self.running())?;
        match run.stage.as_ref()? {
            Running::Measuring { plan, .. } => Some(plan.duration.saturating_sub(self.elapsed())),
            Running::Warmup(_) => None,
        }
    }
}
