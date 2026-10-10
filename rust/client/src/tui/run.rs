//! The run screen as the browser's console: the dial beside the latency lanes, the key and the stage track, over a
//! card per stage; Details over the run, and the stop prompt.
use super::{
    App, Effect, Overlay,
    chrome::Progress,
    console::{self, Arc, Chip, Dial, Lane, ceil_step, gauge_fraction, gauge_tick, gauge_value, icon},
    keys::{Action, Key},
};
use crate::{
    controller::Command,
    events::{Event, Point, Run, Series},
    measure::{format, latency::Population},
    model::{Dir, Direction, ServerFailure, Stage, StageStatus},
    report::{self, vocabulary as words},
    text::{Line, Style},
};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::time::{Duration, Instant};

/// Under this width the dial's readout shows without its ring, and the cards come before the lanes.
const DIAL_MIN: usize = 96;
/// Columns between the dial and what stands beside it.
const GAP: usize = 4;
/// The browser's chart steps (`client/src/lib/presentation/scales.ts`).
const CHART_STEPS: [f64; 10] = [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0];

/// Run-screen state: stage timing, shown rates, each server's latest round trip and timeout streak, the idle latency
/// scale and `l`'s latency pick.
#[derive(Debug, Default)]
pub struct Live {
    since: Option<Instant>,
    shown: Dir<Glide>,
    probes: Vec<(ServerId, Option<Duration>, usize)>,
    /// The dial's scale while idle latency is measured, in milliseconds; it only grows during a run.
    rtt_scale: f64,
    pick: Option<ServerId>,
}

/// A shown rate moving to each new value over the time between samples, as the browser's readout does.
#[derive(Debug, Clone, Copy, Default)]
struct Glide {
    from: f64,
    to: f64,
    at: Option<Instant>,
    over: Duration,
}

impl Glide {
    fn value(&self, now: Instant) -> f64 {
        let Some(at) = self.at.filter(|_| !self.over.is_zero()) else { return self.to };
        let share = now.saturating_duration_since(at).as_secs_f64() / self.over.as_secs_f64();
        self.from + (self.to - self.from) * share.min(1.0)
    }

    /// Glides toward `to` from where the value stands at `now`; a first value snaps.
    fn toward(self, to: f64, now: Instant) -> Self {
        let over = self.at.map_or(Duration::ZERO, |at| {
            now.saturating_duration_since(at)
                .clamp(Duration::from_millis(50), Duration::from_millis(400))
        });
        Self { from: self.value(now), to, at: Some(now), over }
    }
}

impl Live {
    /// Takes `event` once `run` applied it; `shown` is the latency server.
    pub(super) fn event(&mut self, event: &Event, run: Option<&Run>, shown: Option<&ServerId>, now: Instant) {
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
                    let presented = run.live[direction].presented();
                    self.shown[direction] = match rates[direction] {
                        None => Glide::default(),
                        Some(_) if presented > 0.0 => self.shown[direction].toward(presented, now),
                        Some(_) => self.shown[direction],
                    };
                }
            }
            Event::Probe { server, rtt, .. } => {
                let at = self.probes.iter().position(|(probed, ..)| probed == server);
                let at = at.unwrap_or_else(|| {
                    self.probes.push((server.clone(), None, 0));
                    self.probes.len() - 1
                });
                let (_, latest, streak) = &mut self.probes[at];
                (*latest, *streak) = match rtt {
                    Some(rtt) => (Some(*rtt), 0),
                    None => (*latest, *streak + 1),
                };
                let idle = run
                    .and_then(|run| run.stage.as_ref())
                    .is_some_and(|(plan, _)| plan.stage == Stage::Latency);
                if let Some(rtt) = rtt.filter(|_| idle && Some(server) == shown) {
                    let ceiling = ceil_step(rtt.as_secs_f64() * 1e3 * 1.25, &[1.0, 2.0, 4.0]);
                    self.rtt_scale = self.rtt_scale.max(ceiling);
                }
            }
            _ => {}
        }
    }
}

/// Where a stage stands: not begun, warming up, measuring, or ended with its status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Warmup,
    Measuring,
    Ended(StageStatus),
}

impl App {
    pub(super) fn run_key(&mut self, action: Action, key: Key) -> Vec<Effect> {
        match action {
            Action::Stop => {
                self.overlay = Overlay::ConfirmStop;
                self.notice = "Stop the test? esc confirms, any other key continues.".into();
            }
            Action::Details => (self.overlay, self.scroll) = (Overlay::Details, 0),
            Action::Close => (self.overlay, self.scroll) = (Overlay::None, 0),
            Action::Latency => {
                let servers = self.participants();
                let at = servers.iter().position(|id| Some(id) == self.latency_server());
                let next = servers.get(at.map_or(0, |at| (at + 1) % servers.len())).cloned();
                let focus = self.view.run.as_ref().and_then(|run| run.focus.as_ref());
                self.live.pick = next.filter(|next| Some(next) != focus);
            }
            Action::Scroll => (self.scroll, self.follow) = (self.scroll.saturating_add_signed(key.step()), false),
            Action::Again => return self.start(),
            Action::Setup => {
                (self.screen, self.notice, self.scroll, self.setup.row) = (super::Screen::Setup, String::new(), 0, 0);
                self.recheck_soon();
            }
            _ => {}
        }
        Vec::new()
    }

    /// Answers the stop prompt: esc stops the run, q quits, any other key keeps it running.
    pub(super) fn confirm(&mut self, action: Option<Action>) -> Vec<Effect> {
        self.overlay = Overlay::None;
        match action {
            Some(Action::Quit) => self.quit(),
            Some(Action::Abort) => self.interrupt(),
            Some(Action::Confirm) => {
                self.notice = "Stopping the test…".into();
                Effect::command(Command::Stop)
            }
            _ => {
                self.notice = "Test continues.".into();
                Vec::new()
            }
        }
    }

    /// The servers `l` steps through: those prepared that have not left the run.
    fn participants(&self) -> Vec<ServerId> {
        let servers = self.view.servers.iter().filter(|server| self.view.remains(&server.id));
        servers.map(|server| server.id.clone()).collect()
    }

    /// The latency server shown: the one `l` picked while it remains, else the run's.
    pub(super) fn latency_server(&self) -> Option<&ServerId> {
        let focus = self.view.run.as_ref()?.focus.as_ref();
        let pick = self.live.pick.as_ref();
        pick.filter(|pick| self.participants().contains(pick)).or(focus)
    }

    pub(super) fn several(&self) -> bool {
        self.view.servers.len() > 1
    }

    pub(super) fn name(&self, id: Option<&ServerId>) -> String {
        let server = self.view.servers.iter().find(|server| Some(&server.id) == id);
        server.map_or_else(|| words::MISSING.into(), |server| server.name.clone())
    }

    /// How long the stage's warmup or window has run.
    pub(super) fn elapsed(&self) -> Duration {
        let since = self.live.since.unwrap_or(self.now);
        self.now.saturating_duration_since(since)
    }

    /// The progress bar: busy while preparing, then the planned stage time done; an incomplete stage adds nothing.
    pub(super) fn progress(&self) -> Progress {
        let running = self.view.run.as_ref().filter(|_| self.running());
        let Some(run) = running else { return Progress::None };
        if run.at.is_none() {
            return Progress::Busy;
        }
        let done =
            |&(stage, duration): &(Stage, Duration)| match run.results.iter().rfind(|result| result.stage == stage) {
                Some(result) if result.status(run.focus.as_ref()) == StageStatus::Complete => duration,
                None if matches!(&run.stage, Some((plan, Some(_))) if plan.stage == stage) => {
                    self.elapsed().min(duration)
                }
                _ => Duration::ZERO,
            };
        let total: Duration = run.plan.iter().map(|(_, duration)| *duration).sum();
        let done: Duration = run.plan.iter().map(done).sum();
        Progress::Share((done.as_secs_f64() / total.as_secs_f64().max(1.0) * 100.0) as u8)
    }
    /// The run screen's lines: Details over the run, or the console.
    pub(super) fn run_body(&self, width: usize, rows: usize) -> Vec<Line> {
        let Some(run) = &self.view.run else { return Vec::new() };
        if !matches!(self.overlay, Overlay::Details) {
            return self.console(run, width, rows);
        }
        let lines = match run.at {
            None => vec![Line::styled("Waiting for the first server report…", self.palette.muted)],
            Some(_) => report::details(&self.view, width.saturating_sub(4), &self.palette, true),
        };
        self.panel("Details", lines, width, 0)
    }

    /// The dial beside the lanes, the key and the stage track, over the cards; narrow, the readout over the track, the
    /// key's middle row, the cards and the lanes.
    fn console(&self, run: &Run, width: usize, rows: usize) -> Vec<Line> {
        let palette = &self.palette;
        let strip = if rows >= 31 { 4 } else { 2 };
        let cards = self.cards(run, width, strip);
        if width < DIAL_MIN {
            let chips = console::chips(palette, &self.chips(run), width);
            let [_, key, _] = self.run_key_plate(width);
            let mut lines = console::readout(palette, &self.dial(run));
            lines.push(Line::default());
            lines.extend(chips);
            lines.extend([Line::default(), key, Line::default()]);
            lines.extend(cards);
            if let Some(lanes) = self.lanes_card(run, width) {
                lines.push(Line::default());
                lines.extend(lanes);
            }
            return lines;
        }
        let dial_width = 48.min(width * 2 / 5);
        let side = width - dial_width - GAP;
        let mut controls = self.lanes_card(run, side).map(|mut lanes| {
            lanes.push(Line::default());
            lanes
        });
        let controls = controls.get_or_insert_default();
        controls.extend(self.run_key_plate(side));
        controls.push(Line::default());
        controls.extend(console::chips(palette, &self.chips(run), side));
        let height = controls.len().max(rows.saturating_sub(cards.len() + 1).min(19));
        let dial = console::dial(palette, &self.dial(run), dial_width, height);
        let tall = dial.len().max(controls.len());
        let mut lines: Vec<Line> = dial
            .into_iter()
            .chain(std::iter::repeat_with(Line::default))
            .take(tall)
            .zip(controls.drain(..).chain(std::iter::repeat_with(Line::default)))
            .map(|(dial, control)| dial.fit(dial_width).pad(dial_width + GAP).with(control))
            .collect();
        lines.push(Line::default());
        lines.extend(cards);
        lines
    }

    /// Stop while the run goes, then Run again.
    fn run_key_plate(&self, width: usize) -> [Line; 3] {
        match self.running() {
            true => console::key(&self.palette, "Stop", "", "esc", width, true),
            false => console::key(&self.palette, "Run again", &self.plan_time(), "enter", width, true),
        }
    }

    fn state(&self, run: &Run, stage: Stage) -> State {
        let current = run
            .stage
            .as_ref()
            .filter(|(plan, _)| plan.stage == stage && run.outcome.is_none());
        match (current, run.results.iter().rfind(|result| result.stage == stage)) {
            (Some((_, Some(_))), _) => State::Measuring,
            (Some((_, None)), _) => State::Warmup,
            (None, Some(result)) => State::Ended(result.status(run.focus.as_ref())),
            (None, None) => State::Pending,
        }
    }

    /// The latency server's population in `stage` once it has a median.
    fn population(&self, run: &Run, stage: Stage) -> Option<Population> {
        let result = run.results.iter().rfind(|result| result.stage == stage)?;
        let server = result
            .servers
            .iter()
            .find(|server| Some(&server.server) == self.latency_server());
        server?.latency.filter(|population| population.median().is_some())
    }

    /// The latency server's latest round trip.
    fn latest(&self) -> Option<Duration> {
        let shown = self.latency_server();
        self.live
            .probes
            .iter()
            .find(|(id, ..)| Some(id) == shown)
            .and_then(|(_, rtt, _)| *rtt)
    }

    /// Unanswered probes in a row, in the warning tone and from three on the error tone.
    fn timeout_streak(&self) -> Line {
        let shown = self.latency_server();
        match self.live.probes.iter().find(|(id, ..)| Some(id) == shown) {
            Some(&(.., streak @ 1..)) => {
                let style = if streak >= 3 { self.palette.err } else { self.palette.warn };
                Line::styled(format!("probe timeout ×{streak}"), style)
            }
            _ => Line::default(),
        }
    }

    /// The stage's measured window on the plan's clock in seconds, once it opened.
    fn window(&self, run: &Run, stage: Stage) -> Option<(f64, f64)> {
        let at = run.plan.iter().position(|(planned, _)| *planned == stage)?;
        let offset: Duration = run.plan[..at].iter().map(|(_, duration)| *duration).sum();
        let open = matches!(&run.stage, Some((plan, Some(_))) if plan.stage == stage);
        let result = run.results.iter().rfind(|result| result.stage == stage);
        let opened = open || result.is_some_and(|result| !result.measured.is_zero());
        opened.then(|| (offset.as_secs_f64(), (offset + run.plan[at].1).as_secs_f64()))
    }

    /// What the dial shows: the live stage while one measures, the headline result once the run ends.
    fn dial(&self, run: &Run) -> Dial {
        let palette = &self.palette;
        // Before a rate arrives the dial reads against the browser's 100 Mbit/s reference.
        let peak = if run.peak > 0.0 { run.peak } else { 12.5e6 };
        let (shown, unit) = format::tier(peak * 8.0, 1.2);
        let divisor = peak * 8.0 / shown;
        let gauge = gauge_ceiling(peak);
        let scale = self.live.rtt_scale.max(1.0);
        let rate_ticks = std::array::from_fn(|i| gauge_tick(gauge_value(i as f64 / 4.0, gauge) * 8.0 / divisor));
        let ms_ticks: [String; 5] = std::array::from_fn(|i| gauge_tick(i as f64 / 4.0 * scale));
        let label = |stage: Stage| format!("{} {}", icon(stage), words::label(stage));
        let rate = |stage: Stage, bytes_per_sec: f64| Dial {
            arcs: vec![Arc {
                to: gauge_fraction(bytes_per_sec, gauge),
                hue: palette.trace(stage),
            }],
            ticks: rate_ticks.clone(),
            label: label(stage),
            value: format::speed(bytes_per_sec * 8.0 / divisor),
            unit,
            note: Line::default(),
            hue: palette.stage(stage),
        };
        let latency = |rtt: Duration| Dial {
            arcs: vec![Arc {
                to: rtt.as_secs_f64() * 1e3 / scale,
                hue: palette.trace(Stage::Latency),
            }],
            ticks: ms_ticks.clone(),
            label: label(Stage::Latency),
            value: format::latency(rtt.as_secs_f64() * 1e3),
            unit: "ms",
            note: Line::default(),
            hue: palette.stage(Stage::Latency),
        };
        let idle = |label: &str, ticks: [String; 5]| Dial {
            arcs: Vec::new(),
            ticks,
            label: label.into(),
            value: words::MISSING.into(),
            unit: "",
            note: Line::default(),
            hue: palette.muted,
        };
        if self.running() {
            return match &run.stage {
                Some((plan, Some(_))) if plan.stage == Stage::Latency => Dial {
                    note: self.timeout_streak(),
                    ..self
                        .latest()
                        .map_or_else(|| idle(self.status().0, ms_ticks.clone()), latency)
                },
                Some((plan, Some(_))) => {
                    let shown = plan
                        .stage
                        .directions()
                        .iter()
                        .map(|&direction| self.live.shown[direction]);
                    rate(plan.stage, shown.map(|shown| shown.value(self.now)).sum())
                }
                Some((plan, None)) if plan.stage == Stage::Latency => idle(self.status().0, ms_ticks.clone()),
                _ => idle(self.status().0, rate_ticks.clone()),
            };
        }
        // Finished: every transfer stage's arc, the first one's figure.
        let mut means = run.plan.iter().filter_map(|&(stage, _)| {
            let mean = run.results.iter().rfind(|result| result.stage == stage)?.mean();
            (!stage.directions().is_empty() && mean > 0.0).then(|| rate(stage, mean))
        });
        if let Some(mut head) = means.next() {
            head.arcs.extend(means.flat_map(|dial| dial.arcs));
            return head;
        }
        match self
            .population(run, Stage::Latency)
            .and_then(|population| population.median())
        {
            Some(median) => latency(median),
            None => idle(self.status().0, rate_ticks),
        }
    }

    /// The latency lanes as a card, or none when the run measures no latency.
    fn lanes_card(&self, run: &Run, width: usize) -> Option<Vec<Line>> {
        let rows = self.lanes(run);
        if rows.is_empty() {
            return None;
        }
        let mut lines = console::lanes(&self.palette, &rows, width);
        if self.several() {
            let to = format!("To {}, l switches server", self.name(self.latency_server()));
            lines.insert(0, Line::styled(to, self.palette.muted));
        }
        Some(console::card(&self.palette, Stage::Latency, lines, width))
    }

    /// A lane for the idle latency and, with loaded latency on, each transfer stage.
    fn lanes(&self, run: &Run) -> Vec<Lane> {
        let (muted, running) = (self.palette.muted, self.running());
        let loaded = |stage: Stage| stage == Stage::Latency || self.config.loaded_latency;
        let lanes = run.plan.iter().filter(|(stage, _)| loaded(*stage)).map(|&(stage, _)| {
            let note = match self.state(run, stage) {
                State::Measuring => {
                    let now = self
                        .latest()
                        .map_or(String::new(), |rtt| format!(", now {}", words::ms(rtt)));
                    let streak = self.timeout_streak();
                    let gap = if streak.width() > 0 { "  " } else { "" };
                    Line::styled(format!("measuring{now}"), muted)
                        .and(gap, Style::default())
                        .with(streak)
                }
                State::Pending if running => Line::styled("waiting", muted),
                _ if running => Line::styled("next", muted),
                state => Line::styled(unmeasured(state), muted),
            };
            Lane { stage, population: self.population(run, stage), note }
        });
        lanes.collect()
    }

    fn chips(&self, run: &Run) -> Vec<Chip> {
        let palette = &self.palette;
        let chip = |&(stage, duration): &(Stage, Duration)| {
            let (progress, status) = match self.state(run, stage) {
                State::Measuring => {
                    let elapsed = self.elapsed().min(duration);
                    let share = elapsed.as_secs_f64() / duration.as_secs_f64();
                    (share, Line::styled(words::clock(elapsed), palette.text))
                }
                State::Warmup => (0.0, Line::styled(self.spinner(), palette.accent)),
                State::Ended(StageStatus::Complete) => (1.0, Line::styled("✓", palette.ok)),
                State::Ended(StageStatus::Partial) => (1.0, Line::styled("! Partial", palette.warn)),
                State::Ended(StageStatus::Failed) => (0.0, Line::styled("✗ Failed", palette.err)),
                State::Ended(StageStatus::Stopped) => (0.0, Line::styled("Stopped", palette.muted)),
                State::Pending if self.running() => (0.0, Line::styled(words::setting(duration), palette.muted)),
                State::Pending => (0.0, Line::styled("Skipped", palette.muted)),
            };
            Chip { stage, progress, status }
        };
        run.plan.iter().map(chip).collect()
    }

    /// A card per stage, as many to a row as keep each one 28 columns wide.
    fn cards(&self, run: &Run, width: usize, strip: usize) -> Vec<Line> {
        let mut per = run.plan.len();
        while per > 1 && (width - 2 * (per - 1)) / per < 28 {
            per = per.div_ceil(2);
        }
        let card_width = (width - 2 * per.saturating_sub(1)) / per.max(1);
        let mut lines = Vec::new();
        for (row, stages) in run.plan.chunks(per.max(1)).enumerate() {
            if row > 0 {
                lines.push(Line::default());
            }
            let cards: Vec<_> = stages
                .iter()
                .map(|&planned| self.card(run, planned, card_width, strip))
                .collect();
            for index in 0..cards.iter().map(Vec::len).max().unwrap_or(0) {
                let mut line = Line::default();
                for (at, card) in cards.iter().enumerate() {
                    let gap = if at > 0 { "  " } else { "" };
                    let cell = card.get(index).cloned().unwrap_or_default().pad(card_width);
                    line = line.and(gap, Style::default()).with(cell);
                }
                lines.push(line);
            }
        }
        lines
    }

    /// One stage: its figure and state, its strip over its window, and its facts. Its height never changes, so nothing
    /// moves between Start and the result.
    fn card(&self, run: &Run, (stage, duration): (Stage, Duration), width: usize, strip: usize) -> Vec<Line> {
        let palette = &self.palette;
        let state = self.state(run, stage);
        let window = self.window(run, stage);
        let span = window.unwrap_or((0.0, 1.0));
        let result = run.results.iter().rfind(|result| result.stage == stage);
        let population = self.population(run, stage);
        let figure = |text: String| match text.rsplit_once(' ') {
            Some((value, unit)) => Line::styled(value, palette.value).and(format!(" {unit}"), palette.muted),
            None => Line::styled(text, palette.value),
        };
        let (mut value, mut sub) = (Line::styled(words::MISSING, palette.muted), Line::default());
        let (lines, facts) = if stage.directions().is_empty() {
            let mut facts = [
                ("95th percentile", words::MISSING.to_owned()),
                ("Replies", words::MISSING.into()),
                ("Timeouts", words::MISSING.into()),
            ];
            if let Some(population) = population {
                let summary = population.summary;
                value = figure(words::ms(population.median().unwrap_or_default()));
                if let Some(jitter) = summary.jitter.filter(|_| summary.jitter_pairs > 0) {
                    sub = Line::styled(format!("jitter {}", words::ms(jitter)), palette.muted);
                }
                facts[0].1 = summary.p95.map_or(words::MISSING.into(), words::ms);
                facts[1].1 = words::count(summary.replies);
                if let Some(ratio) = summary.timeout_ratio() {
                    facts[2].1 = format!("{:.1}%", ratio * 100.0);
                }
            } else if let Some(rtt) = self.latest().filter(|_| state == State::Measuring) {
                value = figure(words::ms(rtt));
            }
            let series = run
                .rtt
                .iter()
                .find(|(id, _)| Some(id) == self.latency_server())
                .map(|(_, series)| series);
            let points = window.and(series).map_or(&[][..], |series| between(series, span));
            let scale = self.live.rtt_scale.max(1.0);
            (
                console::line(palette, points, scale, span, (width, strip), palette.trace(stage)),
                facts.to_vec(),
            )
        } else {
            let hue = palette.stage(stage);
            let rates: Vec<Line> = stage
                .directions()
                .iter()
                .map(|&direction| {
                    let measured = result.and_then(|result| result.throughput[direction]?.rate);
                    let shown = self.live.shown[direction];
                    match measured {
                        Some(rate) => figure(format::rate(rate.mean)),
                        None if state == State::Measuring && shown.to > 0.0 => {
                            figure(format::rate(shown.value(self.now)))
                        }
                        None => Line::styled(words::MISSING, palette.muted),
                    }
                })
                .collect();
            value = match &rates[..] {
                [down, up] => Line::styled("↓ ", hue)
                    .with(down.clone())
                    .and("  ↑ ", hue)
                    .with(up.clone()),
                _ => rates[0].clone(),
            };
            let bands: Vec<&[Point]> = match window {
                Some(span) => stage
                    .directions()
                    .iter()
                    .map(|&direction| between(&run.throughput[direction], span))
                    .collect(),
                None => Vec::new(),
            };
            let top = ceil_step(run.peak.max(1.0) * 8.0 * 1.03, &CHART_STEPS) / 8.0;
            let lines = console::strip(palette, stage, &bands, top, span, (width, strip));
            let mut facts = match stage.directions().len() {
                1 => vec![("Peak", words::MISSING.to_owned()), ("Transferred", words::MISSING.into())],
                _ => vec![("↓ Transferred", words::MISSING.to_owned()), ("↑ Transferred", words::MISSING.into())],
            };
            facts.push(("Duration", words::MISSING.into()));
            if let Some(result) = result.filter(|result| !result.measured.is_zero()) {
                let throughputs = stage.directions().iter().map(|&direction| result.throughput[direction]);
                for (fact, throughput) in facts.iter_mut().skip(2 - stage.directions().len()).zip(throughputs) {
                    fact.1 = throughput.map_or(words::MISSING.into(), |throughput| format::bytes(throughput.bytes));
                }
                if let [direction] = stage.directions()
                    && let Some(rate) = result.throughput[*direction].and_then(|throughput| throughput.rate)
                    && rate.peak > 0.0
                {
                    facts[0].1 = format::rate(rate.peak);
                }
                facts[2].1 = words::clock(result.measured);
            }
            if let Some(median) = population.and_then(|population| population.median())
                && state == State::Ended(StageStatus::Complete)
            {
                sub = Line::styled(format!("loaded latency {}", words::ms(median)), palette.muted);
            }
            (lines, facts)
        };
        // Why it failed: the latency server's first failure, else any server's.
        let failure = result.and_then(|result| {
            let counted = |failure: &&ServerFailure| failure.failure.reason != FailureReason::InsufficientEvidence;
            let mut failures = result.failures.iter().filter(counted);
            let own = failures
                .clone()
                .find(|failure| Some(&failure.server) == self.latency_server());
            Some(own.or_else(|| failures.next())?.failure.reason.label())
        });
        sub = match state {
            State::Ended(status @ (StageStatus::Failed | StageStatus::Partial)) => {
                let style = if status == StageStatus::Failed { palette.err } else { palette.warn };
                Line::styled(failure.unwrap_or(unmeasured(state)), style)
            }
            State::Ended(StageStatus::Stopped) => Line::styled("Stopped", palette.warn),
            State::Warmup => {
                Line::styled(self.spinner(), palette.accent).and(format!(" {}", self.status().0), palette.muted)
            }
            State::Measuring => Line::styled("measuring", palette.muted),
            State::Pending if self.running() => Line::styled(words::setting(duration), palette.muted),
            State::Pending => Line::styled("Skipped", palette.muted),
            State::Ended(StageStatus::Complete) => sub,
        };
        // A short strip goes without the spacing around it.
        let spacer = (strip >= 4).then(Line::default);
        let mut body = vec![value, sub];
        body.extend(spacer.clone());
        body.extend(lines);
        body.extend(spacer);
        body.extend(
            facts
                .iter()
                .map(|(label, value)| console::fact(palette, label, value, width)),
        );
        console::card(palette, stage, body, width)
    }
}

/// What a stage that measured nothing shows.
fn unmeasured(state: State) -> &'static str {
    match state {
        State::Ended(status) => [words::MISSING, "Partial", "Failed", "Stopped"][status as usize],
        _ => "Skipped",
    }
}

/// The browser's dial scale for a combined rate in bytes per second: its 1-2-5 step above it with 3% headroom, and from
/// a megabit up never under 1 Gbit/s, where most connections fit.
fn gauge_ceiling(bytes_per_sec: f64) -> f64 {
    let bits = bytes_per_sec * 8.0 * 1.03;
    let top = ceil_step(bits, &[1.0, 2.0, 5.0]);
    let top = if bits >= 1e6 { top.max(1e9) } else { top };
    top / 8.0
}

/// The series' points over `window` seconds.
fn between(series: &Series, (t0, t1): (f64, f64)) -> &[Point] {
    let at = |t: f64| series.points.partition_point(|point| point.at.as_secs_f64() < t);
    &series.points[at(t0)..at(t1)]
}
