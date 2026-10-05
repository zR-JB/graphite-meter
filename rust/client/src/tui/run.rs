//! The run screen: the Test panel with the stage track, the timeline's readings and charts, the results once the
//! run finished, the Details overlay and the stop prompt.
use super::{
    App, Effect, Overlay,
    chart::{self, Axis},
    chrome::Progress,
    frame::beside,
    keys::{Action, Key},
};
use crate::{
    controller::Command,
    events::{Event, Run, Series},
    measure::format,
    model::{Dir, Direction, Stage, StageStatus},
    report::{self, vocabulary as words},
    text::{Line, Style},
};
use graphite_meter_proto::catalog::ServerId;
use std::time::{Duration, Instant};

/// How quickly shown rates follow the latest, in seconds.
const EASING: f64 = 0.12;
/// From this width the Test panel stands beside the timeline.
const SIDE: usize = 100;

/// What the run screen keeps beside the view: when the stage's warmup or window began, the eased rates, each
/// server's latest round trip and timeout streak, and the latency server `l` picked.
#[derive(Debug, Default)]
pub struct Live {
    since: Option<Instant>,
    eased: Option<Instant>,
    shown: Dir<Option<f64>>,
    sampled: bool,
    probes: Vec<(ServerId, Option<Duration>, usize)>,
    pick: Option<ServerId>,
}

impl Live {
    pub(super) fn event(&mut self, event: &Event, now: Instant) {
        match event {
            Event::RunStarted { .. } => *self = Self::default(),
            Event::StageStarted(_) => {
                (self.since, self.sampled, self.shown) = (Some(now), false, Dir::default());
                self.probes.clear();
            }
            Event::Measuring(_) => self.since = Some(now),
            Event::Sample { rates, .. } => {
                self.sampled = true;
                for direction in Direction::BOTH {
                    self.shown[direction] = rates[direction].map(|rate| self.shown[direction].unwrap_or(rate));
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
            }
            _ => {}
        }
    }

    /// Moves the shown rates towards `rates` for the time since the last call.
    pub(super) fn ease(&mut self, rates: Dir<Option<f64>>, now: Instant) {
        let elapsed = self.eased.replace(now).map_or(0.0, |eased| (now - eased).as_secs_f64());
        let weight = 1.0 - (-elapsed / EASING).exp();
        for direction in Direction::BOTH {
            let shown = self.shown[direction];
            self.shown[direction] =
                rates[direction].map(|rate| shown.map_or(rate, |shown| shown + (rate - shown) * weight));
        }
    }
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

    /// The progress bar: busy while the run prepares, then the share of planned stage time done, which a stage that
    /// did not complete adds nothing to.
    pub(super) fn progress(&self) -> Progress {
        let Some(run) = self.view.run.as_ref().filter(|_| self.running()) else {
            return Progress::None;
        };
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

    /// The run screen's lines: Details over the run, the results under the timeline once measured, or the stages.
    pub(super) fn run_body(&self, width: usize, rows: usize) -> Vec<Line> {
        let (Some(run), palette) = (&self.view.run, &self.palette) else {
            return Vec::new();
        };
        if matches!(self.overlay, Overlay::Details) {
            let lines = match run.at {
                None => vec![Line::styled("Waiting for the first server report…", palette.muted)],
                Some(_) => report::details(&self.view, width.saturating_sub(4), palette, true),
            };
            return self.panel("Details", lines, width, 0);
        }
        let results = match run.outcome {
            Some(_) => report::results(&self.view, self.latency_server(), width.saturating_sub(4), palette),
            None => Vec::new(),
        };
        if results.is_empty() {
            return self.stages(run, width, rows);
        }
        let title = match self.several() {
            true => format!("Results · latency to {}", self.name(self.latency_server())),
            false => "Results".into(),
        };
        let widest = results.iter().map(Line::width).max().unwrap_or(0);
        let shown = widest.max(crate::text::width(&title) + 2) + 4;
        let bottom = match width.checked_sub(shown + 1).filter(|rest| width >= SIDE && *rest >= 30) {
            Some(rest) => {
                let fields = self.test_fields(run, rest - 4);
                let height = results.len().max(fields.len()) + 2;
                beside(self.panel(&title, results, shown, height), self.panel("Test", fields, rest, height))
            }
            None => self.panel(&title, results, width, 0),
        };
        match rows.saturating_sub(bottom.len()) {
            timeline @ 8.. => [self.timeline(run, width, timeline), bottom].concat(),
            _ => bottom,
        }
    }

    /// The Test panel beside the timeline from 100 cells, else above it; once over, the Test panel alone when the
    /// timeline would not fit.
    fn stages(&self, run: &Run, width: usize, rows: usize) -> Vec<Line> {
        let side = width >= SIDE;
        let left = if side { width * 2 / 5 } else { width };
        let test = self.test_view(run, left - 4, side);
        let height = test.len() + 2;
        let timeline = if side { rows } else { rows.saturating_sub(height) };
        let live = run.outcome.is_none();
        match (side, live || timeline >= 9) {
            (true, true) => {
                let height = timeline.max(height).max(9);
                beside(self.panel("Test", test, left, height), self.timeline(run, width - 1 - left, height))
            }
            (false, true) => [self.panel("Test", test, width, 0), self.timeline(run, width, timeline.max(7))].concat(),
            _ => self.panel("Test", self.test_view(run, width - 4, side), width, 0),
        }
    }

    /// The timeline panel: readings over the charts.
    fn timeline(&self, run: &Run, width: usize, height: usize) -> Vec<Line> {
        let title = match run.outcome {
            None => format!("Timeline · {}", self.status().0),
            Some(_) => "Timeline".into(),
        };
        self.panel(&title, self.live_view(run, width - 4, height - 2), width, height)
    }

    /// The stage's readings while it runs, then the rate chart of its directions and the shown server's latency
    /// chart.
    fn live_view(&self, run: &Run, width: usize, height: usize) -> Vec<Line> {
        let (palette, live) = (&self.palette, run.outcome.is_none());
        let stage = match live {
            true => run.stage.as_ref().map(|(plan, _)| plan.stage),
            false => run.results.last().map(|result| result.stage),
        };
        let Some(stage) = stage else {
            return vec![match live {
                true => self.checking_line(),
                false => Line::styled(words::MISSING, palette.muted),
            }];
        };
        let traced = |direction: &Direction| {
            run.throughput[*direction]
                .points()
                .iter()
                .any(|point| point.value.is_some())
        };
        let directions: Vec<Direction> = match live {
            true => stage.directions().to_vec(),
            false => Direction::BOTH.into_iter().filter(traced).collect(),
        };
        let mut lines = if live { self.readings(run, stage, width) } else { Vec::new() };
        let (marks, end) = marks(run);
        if stage == Stage::Bidirectional || !live && marks.iter().any(|(_, stage)| *stage == Stage::Bidirectional) {
            lines.push(Line::styled("↓ solid · ↑ dashed", palette.muted));
        }
        if self.several() {
            let name = self.name(self.latency_server());
            lines.push(Line::styled(format!("Latency to {name} · l switches server"), palette.muted));
        }
        let span = match live {
            true => run.throughput.down.span(),
            false => Duration::from_secs(end.as_secs_f64().ceil() as u64),
        };
        let empty = Series::default();
        let rtt = run.rtt.iter().find(|(id, _)| Some(id) == self.latency_server());
        let rtt = [(rtt.map_or(&empty, |(_, series)| series), false)];
        let trace = |&direction: &Direction| (&run.throughput[direction], direction == Direction::Up);
        let traces: Vec<_> = directions.iter().map(trace).collect();
        let rates = |height| chart::chart(&traces, &marks, Axis::Rate, span, (width, height), palette);
        let rtts = |height| chart::chart(&rtt, &marks, Axis::Ms, span, (width, height), palette);
        let height = height.saturating_sub(lines.len());
        lines.extend(match height {
            0..5 => Vec::new(),
            _ if directions.is_empty() => rtts(height),
            12.. if self.config.loaded_latency => [rates(height - 5), rtts(5)].concat(),
            _ => rates(height),
        });
        lines
    }

    /// Each direction's eased rate, then the latency the stage measures, wrapped to `width`.
    fn readings(&self, run: &Run, stage: Stage, width: usize) -> Vec<Line> {
        let palette = &self.palette;
        let mut readings = Vec::new();
        for &direction in stage.directions() {
            let value = match (self.live.sampled, run.rates[direction], self.live.shown[direction]) {
                (false, ..) => Line::styled(words::MISSING, palette.muted),
                (true, None, _) => Line::styled(format!("{} window restarting", words::MISSING), palette.muted),
                (true, Some(rate), shown) => Line::styled(format::rate(shown.unwrap_or(rate)), palette.value),
            };
            readings.push(Line::styled(format!("{} ", words::arrow(direction)), palette.stage(stage)).with(value));
        }
        let transfers = !stage.directions().is_empty();
        if !transfers || self.config.loaded_latency {
            let probe = self
                .live
                .probes
                .iter()
                .find(|(id, ..)| Some(id) == self.latency_server());
            let label = if transfers { "Loaded latency " } else { "Idle latency " };
            let mut reading = Line::styled(label, palette.text).with(match probe.and_then(|(_, rtt, _)| *rtt) {
                Some(rtt) => Line::styled(words::ms(rtt), palette.value),
                None => Line::styled(words::MISSING, palette.muted),
            });
            if let Some(&(.., streak @ 1..)) = probe {
                let style = if streak >= 3 { palette.err } else { palette.warn };
                reading = reading.and(format!("  probe timeout ×{streak}"), style);
            }
            readings.push(reading);
        }
        readings.extend(run.recovering.then(|| Line::styled("Recovering", palette.warn)));
        let mut lines = vec![Line::default()];
        for reading in readings {
            let last = lines.last_mut().expect("one line at least");
            match last.width() {
                0 => *last = reading,
                used if used + 3 + reading.width() > width => lines.push(reading),
                _ => *last = std::mem::take(last).and("   ", Style::default()).with(reading),
            }
        }
        lines
    }
}

/// Where each stage that measured began on the plan's clock, and where measuring ended so far.
fn marks(run: &Run) -> (Vec<(Duration, Stage)>, Duration) {
    let (mut marks, mut offset, mut end) = (Vec::new(), Duration::ZERO, Duration::ZERO);
    for &(stage, duration) in &run.plan {
        let result = run.results.iter().rfind(|result| result.stage == stage);
        let measured = result.map(|result| result.measured);
        let open = matches!(&run.stage, Some((plan, Some(_))) if plan.stage == stage);
        if open || measured.is_some_and(|measured| !measured.is_zero()) {
            marks.push((offset, stage));
            end = end.max(offset + measured.unwrap_or(duration));
        }
        offset += duration;
    }
    (marks, end.max(Duration::from_secs(1)))
}
