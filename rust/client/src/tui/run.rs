//! The run screen: Test panel and stage track, timeline readings and braille charts, results, Details, stop prompt.
use super::{
    App, Effect, Overlay,
    chrome::Progress,
    frame::{beside, unique},
    keys::{Action, Key},
    theme::Palette,
};
use crate::{
    controller::Command,
    events::{Event, Point, Run, Series},
    measure::format,
    model::{Dir, Direction, Stage, StageResult, StageStatus},
    report::{self, vocabulary as words},
    run::prepare::Paths,
    text::{Line, Style, wrap},
};
use graphite_meter_proto::catalog::ServerId;
use std::time::{Duration, Instant};

/// How quickly shown rates follow the latest, in seconds.
const EASING: f64 = 0.12;
/// From this width the Test panel stands beside the timeline.
const SIDE: usize = 100;

/// Run-screen state: stage timing, eased rates, each server's latest round trip and timeout streak, `l`'s latency pick.
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

    /// The run screen's lines: Details over the run, the results under the timeline once measured, or the stages.
    pub(super) fn run_body(&self, width: usize, rows: usize) -> Vec<Line> {
        let Some(run) = &self.view.run else { return Vec::new() };
        let palette = &self.palette;
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

    /// The Test panel beside the timeline from 100 cells, else above; once over, alone when the timeline would not fit.
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

    /// The stage's readings while it runs, then its directions' rate chart and the shown server's latency chart.
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
            let points = &run.throughput[*direction].points;
            points.iter().any(|point| point.value.is_some())
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
            true => run.throughput.down.span,
            false => Duration::from_secs(end.as_secs_f64().ceil() as u64),
        };
        let empty = Series::default();
        let rtt = run.rtt.iter().find(|(id, _)| Some(id) == self.latency_server());
        let rtt = [(rtt.map_or(&empty, |(_, series)| series), false)];
        let trace = |&direction: &Direction| (&run.throughput[direction], direction == Direction::Up);
        let traces: Vec<_> = directions.iter().map(trace).collect();
        let rates = |height| chart(&traces, &marks, Axis::Rate, span, (width, height), palette);
        let rtts = |height| chart(&rtt, &marks, Axis::Ms, span, (width, height), palette);
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
            let shown = self.latency_server();
            let probe = self.live.probes.iter().find(|(id, ..)| Some(id) == shown);
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

const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

impl App {
    /// The stage track, under the run's settings when `fields`.
    pub(super) fn test_view(&self, run: &Run, width: usize, fields: bool) -> Vec<Line> {
        match fields {
            true => [self.test_fields(run, width), vec![Line::default()], self.track(run, width)].concat(),
            false => self.track(run, width),
        }
    }

    /// The run's servers, paths, streams and timing.
    pub(super) fn test_fields(&self, run: &Run, width: usize) -> Vec<Line> {
        let (palette, config) = (&self.palette, &self.config);
        let label = |name: &str| Line::styled(format!("{name:<11}"), palette.text);
        if run.at.is_none() {
            let value = match run.outcome {
                None => self.checking_line(),
                Some(_) => Line::styled(words::MISSING, palette.muted),
            };
            return vec![label("Servers").with(value)];
        }
        let servers = self.view.servers.iter();
        let paths: Vec<_> = servers
            .filter_map(|server| Some((&server.id, server.path.as_ref().ok()?)))
            .collect();
        let throughput = unique(paths.iter().map(|(_, paths)| words::throughput_path(&paths.throughput)));
        let shown = paths.iter().find(|(id, _)| Some(*id) == self.latency_server());
        let latency = shown.and_then(|(_, paths)| paths.latency.as_ref());
        let streams = |(_, paths): &(_, &Paths)| words::streams(config, &paths.throughput);
        let streams = paths.last().map_or(words::MISSING.into(), streams);
        let names: Vec<_> = self.view.servers.iter().map(|server| server.name.as_str()).collect();
        let (names, streams) = match self.several() {
            true => (format!("{} (all servers)", names.join(", ")), format!("per server · {streams}")),
            false => (names.join(", "), streams),
        };
        let (warmup, idle, loaded) = (
            words::setting(config.warmup),
            words::cadence(config.ping),
            words::cadence(config.loaded_ping),
        );
        let fields = [
            ("Servers", names),
            ("Throughput", throughput.join(" / ")),
            ("Latency", latency.map_or(words::MISSING.into(), words::latency_path)),
            ("Streams", streams),
            ("Timing", format!("warmup {warmup} · latency cadence {idle} · loaded cadence {loaded}")),
        ];
        let mut lines = Vec::new();
        for (name, value) in fields {
            let parts: Vec<String> = value.split(" · ").map(str::to_owned).collect();
            for (index, part) in wrap(&parts, width.saturating_sub(11).max(12)).into_iter().enumerate() {
                lines.push(label(if index == 0 { name } else { "" }).and(part, palette.value));
            }
        }
        lines
    }

    /// Each planned stage: waiting, its warmup, its window's progress, or how it ended.
    fn track(&self, run: &Run, width: usize) -> Vec<Line> {
        let (palette, live) = (&self.palette, run.outcome.is_none());
        let current = run.stage.as_ref().filter(|_| live);
        let current = current.map(|(plan, window)| (plan.stage, window.is_some()));
        let mut lines = Vec::new();
        for &(stage, duration) in &run.plan {
            let (hue, muted) = (palette.stage(stage), palette.muted);
            let result = run.results.iter().rfind(|result| result.stage == stage);
            let state = match result.map(|result| (result, result.status(run.focus.as_ref()))) {
                Some((result, StageStatus::Complete)) => {
                    let headline = Some(self.headline(result)).filter(|headline| headline.width() > 0);
                    let headline = headline.unwrap_or_else(|| Line::styled(words::setting(duration), muted));
                    Line::styled("✓ ", palette.ok).with(headline)
                }
                Some((result, StageStatus::Partial)) => {
                    let headline = self.headline(result);
                    let gap = if headline.width() > 0 { " " } else { "" };
                    Line::styled("! ", palette.warn)
                        .with(headline)
                        .and(format!("{gap}Partial"), muted)
                }
                Some((_, StageStatus::Failed)) => Line::styled("✗ ", palette.err).and("Failed", muted),
                Some((_, StageStatus::Stopped)) => Line::styled("○ Stopped", muted),
                None => match current.filter(|(current, _)| *current == stage) {
                    Some((_, true)) => {
                        let (elapsed, cells) = (self.elapsed(), width.saturating_sub(34).clamp(6, 30));
                        bar(hue, elapsed.as_secs_f64() / duration.as_secs_f64(), cells, muted)
                            .and("  ", Style::default())
                            .and(words::clock(elapsed.min(duration)), palette.value)
                            .and(format!(" / {}", words::setting(duration)), muted)
                    }
                    Some((_, false)) => Line::styled(self.spinner(), palette.accent)
                        .and(" warmup ", muted)
                        .and(words::clock(self.elapsed()), palette.value),
                    None if live => Line::styled(format!("○ {}", words::setting(duration)), muted),
                    None => Line::styled(format!("{} Skipped", words::MISSING), muted),
                },
            };
            lines.push(Line::styled(format!("{:<14}", words::label(stage)), hue).with(state));
        }
        lines
    }

    /// A finished stage's mean rates, and for the latency stage the shown server's median.
    fn headline(&self, result: &StageResult) -> Line {
        let mut line = Line::styled(words::mean_rates(result), self.palette.value);
        let shown = self.latency_server();
        let own = result.servers.iter().find(|own| Some(&own.server) == shown);
        let median = own.and_then(|own| own.latency?.median());
        let median = median.filter(|_| result.stage == Stage::Latency);
        if let Some(median) = median {
            let gap = if line.width() > 0 { "  " } else { "" };
            line = line
                .and(format!("{gap}{}", words::ms(median)), self.palette.value)
                .and(" median", self.palette.muted);
        }
        line
    }
}

/// `share` of `width` cells filled to an eighth, over the rest in shade.
fn bar(fill: Style, share: f64, width: usize, rest: Style) -> Line {
    let cells = (share * width as f64).clamp(0.0, width as f64);
    let (full, part) = (cells as usize, EIGHTHS[((cells.fract()) * 8.0) as usize]);
    let shade = width - full - usize::from(!part.is_empty());
    Line::styled(format!("{}{part}", "█".repeat(full)), fill).and("░".repeat(shade), rest)
}

/// The scale's columns left of the axis.
const SCALE: usize = 10;
/// The dot of each row and column within a braille cell.
const DOTS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// What a chart's values measure: rates in bytes per second, labelled in bits, or round trips in milliseconds.
#[derive(Debug, Clone, Copy)]
pub enum Axis {
    Rate,
    Ms,
}

impl Axis {
    /// The factor from values to labels.
    fn scale(self) -> f64 {
        if let Self::Rate = self { 8.0 } else { 1.0 }
    }

    /// `value` in the label's unit, the largest it reaches for rates.
    fn label(self, value: f64) -> String {
        let round = |value: f64| (value * 1000.0).round() / 1000.0;
        match self {
            Self::Rate => {
                let (value, unit) = format::tier(value, 1.0);
                format!("{} {unit}", round(value))
            }
            Self::Ms => format!("{} ms", round(value)),
        }
    }
}

/// A trace: its series, and whether its stretch in a bidirectional stage is dashed.
pub type Trace<'a> = (&'a Series, bool);

/// `traces` over `span` in `width` × `height` cells, with `marks` where stages began.
pub fn chart(
    traces: &[Trace],
    marks: &[(Duration, Stage)],
    axis: Axis,
    span: Duration,
    (width, height): (usize, usize),
    palette: &Palette,
) -> Vec<Line> {
    let (columns, rows) = (width.saturating_sub(SCALE + 1).max(4), height.saturating_sub(2).max(2));
    let stretches: Vec<_> = traces.iter().flat_map(|trace| stretches(trace, marks)).collect();
    let values = stretches.iter().flat_map(|stretch| stretch.2);
    let peak = values.fold(0.0_f64, |peak, point| peak.max(point.peak).max(point.value.unwrap_or(0.0)));
    let top = nice(peak * axis.scale() * 1.05) / axis.scale();
    let mut canvas = Canvas { columns, rows, cells: vec![(0, 0); columns * rows] };
    let span = span.as_secs_f64().max(1.0);
    for (index, (_, dashed, points)) in stretches.iter().enumerate() {
        canvas.plot(index, *dashed, points, span, top);
    }
    let mut lines = Vec::new();
    for row in 0..rows {
        let middle = row == rows / 2 && rows >= 6;
        let scale = match row {
            0 if peak > 0.0 => axis.label(top * axis.scale()),
            _ if row == rows - 1 => "0".into(),
            _ if middle && peak > 0.0 => axis.label(top * axis.scale() / 2.0),
            _ => String::new(),
        };
        let scale: String = scale.chars().take(SCALE).collect();
        let mut line = Line::styled(format!("{scale:>SCALE$}"), palette.muted).and("│", palette.border);
        let cells: Vec<usize> = (row * columns..(row + 1) * columns).collect();
        let owned = |&a: &usize, &b: &usize| {
            let ((a, from), (b, to)) = (canvas.cells[a], canvas.cells[b]);
            (a == 0) == (b == 0) && from == to
        };
        for run in cells.chunk_by(owned) {
            let glyph = |&cell: &usize| char::from_u32(0x2800 + u32::from(canvas.cells[cell].0)).unwrap_or(' ');
            let glyphs: String = run.iter().map(glyph).collect();
            line = match canvas.cells[run[0]] {
                (0, _) if middle => line.and("┄".repeat(run.len()), palette.border),
                (0, _) => line.and(" ".repeat(run.len()), Style::default()),
                (_, owner) => line.and(glyphs, palette.trace(stretches[owner].0)),
            };
        }
        lines.push(line);
    }
    lines.extend(ruler(marks, span, columns, palette));
    lines
}

/// The ruler with a tick where each stage began, and beneath it their names and the span's end.
fn ruler(marks: &[(Duration, Stage)], span: f64, columns: usize, palette: &Palette) -> [Line; 2] {
    let end = words::clock(Duration::from_secs_f64(span));
    let end: String = end.chars().take(columns).collect();
    let end_at = columns - end.chars().count();
    let column = |at: Duration| ((at.as_secs_f64() / span * columns as f64) as usize).min(columns - 1);
    let (mut ticks, mut names, mut written) = (vec!['─'; columns], Line::plain(" ".repeat(SCALE + 1)), 0);
    for (index, &(at, stage)) in marks.iter().enumerate() {
        let x = column(at);
        ticks[x] = '┬';
        let following = marks.get(index + 1);
        let next = following.map_or(end_at, |&(next, _)| column(next).min(end_at));
        let room = next.saturating_sub(x + 1);
        if room >= 3 && x >= written {
            let name = Line::styled(words::compact_stage(stage), palette.stage(stage)).fit(room);
            (names, written) = (names.and(" ".repeat(x - written), Style::default()), x + name.width());
            names = names.with(name);
        }
    }
    let names = names.and(" ".repeat(end_at.saturating_sub(written)), Style::default());
    [
        Line::plain(" ".repeat(SCALE)).and(format!("└{}", ticks.into_iter().collect::<String>()), palette.border),
        names.and(end, palette.muted),
    ]
}

/// A trace's points split at each mark into its stage's stretch; the upload's in a bidirectional stage is dashed.
fn stretches<'a>(&(series, upload): &Trace<'a>, marks: &[(Duration, Stage)]) -> Vec<(Stage, bool, &'a [Point])> {
    let (mut points, mut stretches) = (&series.points[..], Vec::new());
    for &(at, stage) in marks.iter().rev() {
        let start = points.partition_point(|point| point.at < at);
        stretches.push((stage, upload && stage == Stage::Bidirectional, &points[start..]));
        points = &points[..start];
    }
    stretches.reverse();
    stretches
}

/// The smallest of 1, 2, 2.5 and 5 times a power of ten that reaches `value`.
fn nice(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let decade = 10_f64.powf(value.log10().floor());
    let mut steps = [1.0, 2.0, 2.5, 5.0].into_iter().map(|step| step * decade);
    steps.find(|ceiling| *ceiling >= value).unwrap_or(10.0 * decade)
}

/// Braille cells and the stretch that drew each last.
struct Canvas {
    columns: usize,
    rows: usize,
    cells: Vec<(u8, usize)>,
}

impl Canvas {
    /// Draws `points` as stretch `owner`: a dot column's values merge into their mean, lines join them, gaps break.
    fn plot(&mut self, owner: usize, dashed: bool, points: &[Point], span: f64, top: f64) {
        let (width, height) = (self.columns * 2, self.rows * 4);
        let column = |point: &Point| ((point.at.as_secs_f64() / span * width as f64) as usize).min(width - 1);
        let (mut last, mut points) = (None, points.iter().peekable());
        while let Some(point) = points.next() {
            let Some(value) = point.value else {
                last = None;
                continue;
            };
            let x = column(point);
            let (mut sum, mut count) = (value * f64::from(point.count), f64::from(point.count));
            while let Some(next) = points.next_if(|next| next.value.is_some() && column(next) == x) {
                sum += next.value.unwrap_or(0.0) * f64::from(next.count);
                count += f64::from(next.count);
            }
            let share = (sum / count / top).clamp(0.0, 1.0);
            let y = height - 1 - (share * (height - 1) as f64).round() as usize;
            self.line(last.unwrap_or((x, y)), (x, y), owner, dashed);
            last = Some((x, y));
        }
    }

    /// A line of dots from `from` to `to`, every third pair of columns left out when `dashed`.
    fn line(&mut self, (mut x, mut y): (usize, usize), (x1, y1): (usize, usize), owner: usize, dashed: bool) {
        let (dx, dy) = (x1.abs_diff(x) as isize, -(y1.abs_diff(y) as isize));
        let (mut error, step_x, step_y) = (dx + dy, if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
        loop {
            if !(dashed && x % 6 >= 4) {
                let cell = &mut self.cells[y / 4 * self.columns + x / 2];
                *cell = (cell.0 | DOTS[y % 4][x % 2], owner);
            }
            if (x, y) == (x1, y1) {
                return;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                (error, x) = (error + dy, x.saturating_add_signed(step_x));
            }
            if doubled <= dx {
                (error, y) = (error + dx, y.saturating_add_signed(step_y));
            }
        }
    }
}
