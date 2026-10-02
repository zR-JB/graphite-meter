//! Go's run view (view.go and ui.go): the stage track, the timeline's charts over the run's time,
//! and the results with the test's settings.
use super::{
    Ui,
    view::{TWO_COLUMN_MIN, columns, join, panel},
};
use crate::{
    config::Config,
    model::{Phase, Snapshot, Stage, StageStatus},
    report::{ARROWS, Report, Text, directions, fit, line, plain, run_servers, server_name, span, wrap_parts},
    theme::Theme,
    vocabulary::{self as words, MISSING, compact_stage},
};
use graphite_meter_core::format;
use ratatui_core::{
    style::Style,
    text::{Line, Span},
};
use std::{collections::HashMap, time::Duration};
use tokio::time::Instant;
use unicode_width::UnicodeWidthStr;

/// Go's historyPoints and traceStep.
const HISTORY_POINTS: usize = 480;
const TRACE_STEP: f64 = 0.05;
/// Go's chartAxis: the scale's columns and the axis.
const CHART_AXIS: usize = 11;
const BRAILLE: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Go's trace: samples averaged within a step, which doubles as the history coarsens; NaN is a gap.
#[derive(Clone, Debug, Default)]
pub(super) struct Trace {
    points: Vec<(f64, f64, usize, f64)>,
    step: f64,
}

impl Trace {
    /// Adds the mean of `count` samples; one from before the last point, such as replies ahead of a
    /// stage's mark, joins that point.
    pub(super) fn add(&mut self, at: f64, value: f64, count: usize) {
        if !at.is_finite() || value.is_infinite() || count == 0 {
            return;
        }
        self.step = self.step.max(TRACE_STEP);
        let at = self.points.last().map_or(at, |last| at.max(last.0));
        if let Some(last) = self.points.last_mut()
            && at - last.0 < self.step
            && !value.is_nan()
            && !last.1.is_nan()
        {
            last.3 = last.3.max(value);
            last.2 += count;
            last.1 += (value - last.1) * count as f64 / last.2 as f64;
            return;
        }
        if self.points.len() == HISTORY_POINTS {
            // Go's coarsen: pairs merge, and a gap in either keeps the pair a gap.
            let len = self.points.len().div_ceil(2);
            for index in 0..len {
                let mut point = self.points[index * 2];
                if let Some(next) = self.points.get(index * 2 + 1) {
                    point.3 = point.3.max(next.3);
                    if next.1.is_nan() {
                        point.1 = next.1;
                    } else if !point.1.is_nan() {
                        point.1 = (point.1 * point.2 as f64 + next.1 * next.2 as f64) / (point.2 + next.2) as f64;
                        point.2 += next.2;
                    }
                }
                self.points[index] = point;
            }
            self.points.truncate(len);
            self.step *= 2.0;
        }
        self.points.push((at, value, count, value.max(0.0)));
    }
}

/// What Go's runState keeps for the view: the run's clock, stage marks, the traces its charts
/// draw, the latest round trips and the smoothed rates.
#[derive(Clone, Default)]
pub(super) struct Run {
    pub config: Config,
    started: Option<Instant>,
    ended: Option<Instant>,
    eased: Option<Instant>,
    planned_span: f64,
    marks: Vec<(f64, Stage)>,
    /// The download and upload rates.
    traces: [Trace; 2],
    rtt: HashMap<String, Trace>,
    /// Each server's last reply in this stage, in milliseconds.
    latest: HashMap<String, f64>,
    step: Option<(Option<Stage>, Phase)>,
    pub since: Option<Instant>,
    sample: Option<Duration>,
    pub shown: [Option<f64>; 2],
}

impl Run {
    pub(super) fn new(config: Config) -> Self {
        let now = Instant::now();
        let planned_span = config
            .stages
            .iter()
            .map(|stage| (config.duration(*stage) + config.warmup).as_secs_f64())
            .sum();
        Self {
            config,
            started: Some(now),
            eased: Some(now),
            planned_span,
            ..Self::default()
        }
    }

    /// Go's apply: stage events set the marks and clear the stage's readings; samples extend the traces.
    pub(super) fn observe(&mut self, snapshot: &Snapshot) {
        let now = Instant::now();
        let at = clock(self.started, now);
        let step = (snapshot.stage, snapshot.phase);
        if self.step != Some(step) {
            if snapshot.phase == Phase::Preparing {
                self.latest.clear();
                self.shown = [None; 2];
            }
            if let (Some(stage), Phase::Measuring) = step {
                self.marks.push((at, stage));
                // Go's history and rtt maps hold a trace from its first point on, and only those take the gap.
                let traces = self.traces.iter_mut().chain(self.rtt.values_mut());
                for trace in traces.filter(|trace| !trace.points.is_empty()) {
                    trace.add(at, f64::NAN, 1);
                }
            }
            (self.step, self.since, self.sample) = (Some(step), Some(now), None);
        }
        let latest = &snapshot.latest;
        if snapshot.phase == Phase::Measuring && latest.sample_count > 0 && self.sample != Some(latest.elapsed) {
            self.sample = Some(latest.elapsed);
            let rates = [latest.down_bps, latest.up_bps];
            for index in directions(snapshot.stage.unwrap_or_default()) {
                self.traces[index].add(at, rates[index].map_or(f64::NAN, |bits| bits / 8.0), 1);
                if rates[index].is_none() || self.shown[index].is_none() {
                    self.shown[index] = rates[index];
                }
            }
            // Go's chart takes every reply, as a mean per step, and each timeout as a gap.
            for host in &snapshot.server_latencies {
                self.latest.extend(host.latest_ms.map(|ms| (host.id.clone(), ms)));
                let trace = self.rtt.entry(host.id.clone()).or_default();
                for (step, ms, count) in &host.steps {
                    trace.add(clock(self.started, *step), ms * 1e6, *count);
                }
            }
        }
        if !snapshot.phase.live() {
            self.ended.get_or_insert(now);
            self.shown = [latest.down_bps, latest.up_bps];
        }
    }

    /// Go's frame tick: the shown rates ease towards the latest.
    pub(super) fn ease(&mut self, snapshot: &Snapshot) {
        let now = Instant::now();
        let dt = self
            .eased
            .replace(now)
            .map_or(0.0, |previous| now.saturating_duration_since(previous).as_secs_f64());
        let weight = 1.0 - (-dt / 0.12).exp();
        let targets = [snapshot.latest.down_bps, snapshot.latest.up_bps];
        for (shown, target) in self.shown.iter_mut().zip(targets) {
            *shown = target.map(|target| shown.map_or(target, |value| value + (target - value) * weight));
        }
    }

    /// How long the current stage phase has run, as Go's now minus the stage's since.
    fn elapsed(&self) -> Duration {
        self.since.map_or(Duration::ZERO, |since| since.elapsed())
    }

    /// Go's span: the run's time so far, or until it ended.
    fn span(&self) -> f64 {
        match self.ended {
            Some(ended) => clock(self.started, ended).ceil().max(1.0),
            None => self
                .planned_span
                .max((clock(self.started, Instant::now()) / 10.0).ceil() * 10.0)
                .max(1.0),
        }
    }

    /// Go's progress: the planned stage time done, where a partial or failed stage counts nothing.
    pub(super) fn progress(&self, snapshot: &Snapshot) -> u8 {
        let (mut done, mut total) = (Duration::ZERO, Duration::ZERO);
        for stage in &self.config.stages {
            let duration = self.config.duration(*stage);
            total += duration;
            if let Some(result) = snapshot.results.iter().rfind(|result| result.stage == *stage) {
                if snapshot.stage_status(result) == StageStatus::Complete {
                    done += duration;
                }
            } else if snapshot.stage == Some(*stage) && snapshot.phase == Phase::Measuring {
                done += self.elapsed().min(duration);
            }
        }
        (done.as_secs_f64() / total.as_secs_f64().max(1.0) * 100.0) as u8
    }
}

/// Seconds of the run's clock from its start to `at`.
fn clock(started: Option<Instant>, at: Instant) -> f64 {
    started.map_or(0.0, |started| at.saturating_duration_since(started).as_secs_f64())
}

/// Go's axis: the unit its values scale to and the label of the top.
#[derive(Clone, Copy)]
struct Axis {
    scale: f64,
    label: fn(f64) -> String,
}

/// Go's roundLabel.
fn round_label(value: f64) -> String {
    ((value * 1000.0).round() / 1000.0).to_string()
}

/// Go's rateAxis: the top in the largest unit it reaches, as Go's rateTier without headroom.
const RATE_AXIS: Axis = Axis {
    scale: 8.0,
    label: |bits| {
        let units = ["bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"];
        let tier = (1..5).take_while(|tier| bits >= 1e3_f64.powi(*tier)).count();
        format!("{} {}", round_label(bits / 1e3_f64.powi(tier as i32)), units[tier])
    },
};
const MS_AXIS: Axis = Axis {
    scale: 1e-6,
    label: |ms| format!("{} ms", round_label(ms)),
};

/// Go's niceCeil.
fn nice_ceil(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let decade = 10f64.powf(value.log10().floor());
    let mut ceilings = [1.0, 2.0, 2.5, 5.0].into_iter().map(|step| step * decade);
    ceilings.find(|ceiling| *ceiling >= value).unwrap_or(10.0 * decade)
}

/// A chart line: its stage's style and its points.
type Series<'a> = (Style, &'a [(f64, f64, usize, f64)], bool);

/// Go's stageSeries: the points after each mark, in its stage's style.
fn stage_series<'a>(
    mut points: &'a [(f64, f64, usize, f64)],
    marks: &[(f64, Stage)],
    theme: &Theme,
    upload: bool,
) -> Vec<Series<'a>> {
    let mut out = vec![(Style::new(), &points[..0], false); marks.len()];
    for (index, (at, stage)) in marks.iter().enumerate().rev() {
        let start = points.partition_point(|point| point.0 < *at);
        out[index] = (
            theme.trace(*stage),
            &points[start..],
            upload && *stage == Stage::Bidirectional,
        );
        points = &points[..start];
    }
    out
}

/// Go's chart: braille lines over a scale, a time ruler with the stage marks, and the span.
fn chart(
    lines: &[Series],
    marks: &[(f64, Stage)],
    axis: Axis,
    span_: f64,
    width: usize,
    height: usize,
    theme: &Theme,
) -> Text {
    let (cols, rows) = (width.saturating_sub(CHART_AXIS).max(4), height.saturating_sub(2).max(2));
    let (t0, t1) = (0.0, span_.max(1.0));
    // f64::max passes over the gaps.
    let peak = lines
        .iter()
        .flat_map(|(_, points, _)| points.iter().map(|point| point.3));
    let peak = peak.fold(0.0_f64, f64::max);
    let top = nice_ceil(peak * axis.scale * 1.05) / axis.scale;
    let (dot_width, dot_height) = (cols * 2, rows * 4);
    let (mut dots, mut owner) = (vec![0u8; cols * rows], vec![0usize; cols * rows]);
    let mut set = |x: isize, y: isize, series: usize| {
        let (x, y) = (x as usize, y as usize);
        if lines[series].2 && x % 6 >= 4 {
            return;
        }
        let cell = y / 4 * cols + x / 2;
        dots[cell] |= BRAILLE[y % 4][x % 2];
        owner[cell] = series;
    };
    let mut segment = |(mut x0, mut y0): (isize, isize), (x1, y1): (isize, isize), series: usize| {
        let (dx, sx) = ((x1 - x0).abs(), (x1 - x0).signum());
        let (dy, sy) = (-(y1 - y0).abs(), (y1 - y0).signum());
        let mut error = dx + dy;
        loop {
            set(x0, y0, series);
            if x0 == x1 && y0 == y1 {
                return;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                (error, x0) = (error + dy, x0 + sx);
            }
            if doubled <= dx {
                (error, y0) = (error + dx, y0 + sy);
            }
        }
    };
    let column = |at: f64| (((at - t0) / (t1 - t0) * dot_width as f64) as isize).clamp(0, dot_width as isize - 1);
    for (index, (_, points, _)) in lines.iter().enumerate() {
        // Average adjacent samples in a dot column; a gap breaks the line, even within that column.
        let mut points = points.iter().peekable();
        let mut last = None;
        while let Some(point) = points.next() {
            if point.1.is_nan() {
                last = None;
                continue;
            }
            let x = column(point.0);
            let (mut sum, mut count) = (point.1 * point.2 as f64, point.2);
            while let Some(next) = points.next_if(|point| !point.1.is_nan() && column(point.0) == x) {
                sum += next.1 * next.2 as f64;
                count += next.2;
            }
            let share = (sum / count as f64 / top).clamp(0.0, 1.0);
            let y = dot_height as isize - 1 - (share * (dot_height - 1) as f64).round() as isize;
            segment(last.unwrap_or((x, y)), (x, y), index);
            last = Some((x, y));
        }
    }
    let mut out = Vec::new();
    for row in 0..rows {
        let scale = match row {
            0 if peak > 0.0 => (axis.label)(top * axis.scale),
            row if row == rows - 1 => "0".into(),
            row if row == rows / 2 && rows >= 6 && peak > 0.0 => (axis.label)(top * axis.scale / 2.0),
            _ => String::new(),
        };
        let scale: String = scale.chars().take(CHART_AXIS - 1).collect();
        let mut spans = vec![
            span(format!("{scale:>width$}", width = CHART_AXIS - 1), theme.muted),
            span("│", theme.border),
        ];
        let mut column = 0;
        while column < cols {
            let first = row * cols + column;
            let mut end = first;
            while end < (row + 1) * cols && owner[end] == owner[first] && (dots[end] == 0) == (dots[first] == 0) {
                end += 1;
            }
            if dots[first] == 0 {
                spans.push(if row == rows / 2 && rows >= 6 {
                    span("┄".repeat(end - first), theme.border)
                } else {
                    Span::raw(" ".repeat(end - first))
                });
            } else {
                let glyphs: String = dots[first..end]
                    .iter()
                    .map(|bits| char::from_u32(0x2800 + u32::from(*bits)).unwrap_or(' '))
                    .collect();
                spans.push(span(glyphs, lines[owner[first]].0));
            }
            column += end - first;
        }
        out.push(Line::from(spans));
    }
    let mut ruler: Vec<char> = "─".repeat(cols).chars().collect();
    let end: String = words::clock(Duration::from_secs_f64(t1)).chars().take(cols).collect();
    let end_at = cols.saturating_sub(end.chars().count());
    let column = |at: f64| (((at - t0) / (t1 - t0) * cols as f64) as isize).min(cols as isize - 1);
    let (mut labels, mut written) = (Vec::new(), 0);
    for (index, (at, stage)) in marks.iter().enumerate() {
        let (x, mut limit) = (column(*at), end_at as isize - 1);
        if x < 0 || x >= cols as isize {
            continue;
        }
        if let Some((next, _)) = marks.get(index + 1) {
            limit = limit.min(column(*next) - 1);
        }
        ruler[x as usize] = '┬';
        let room = limit - x;
        if room >= 3 {
            let label = plain(&fit(Line::from(compact_stage(*stage)), room as usize));
            labels.push(Span::raw(" ".repeat((x as usize).saturating_sub(written))));
            written = x as usize + label.width();
            labels.push(span(label, theme.stage(*stage)));
        }
    }
    let ruler = span(format!("└{}", ruler.into_iter().collect::<String>()), theme.border);
    out.push(Line::from(vec![Span::raw(" ".repeat(CHART_AXIS - 1)), ruler]));
    let (mut last, gap) = (vec![Span::raw(" ".repeat(CHART_AXIS))], end_at.saturating_sub(written));
    last.extend(labels);
    last.extend([Span::raw(" ".repeat(gap)), span(end, theme.muted)]);
    out.push(Line::from(last));
    out
}

/// Go's bar: whole cells and an eighth, over the rest in shade.
fn bar(fill: Style, value: f64, scale: f64, width: usize, theme: &Theme) -> Vec<Span<'static>> {
    let share = if scale > 0.0 { value / scale } else { 0.0 };
    let cells = (share * width as f64).clamp(0.0, width as f64);
    let full = cells as usize;
    let part = EIGHTHS[((cells - full as f64) * 8.0) as usize];
    let rest = width - full - usize::from(!part.is_empty());
    vec![
        span(format!("{}{part}", "█".repeat(full)), fill),
        span("░".repeat(rest), theme.muted),
    ]
}

impl Ui {
    /// Go's runView: while live, or without results, the stage view; after a run, its results.
    pub(super) fn run_view(&self, width: usize, height: usize) -> Text {
        let Some((snapshot, run)) = self.shown() else {
            return Vec::new();
        };
        let theme = &self.theme;
        let results = match snapshot.phase.live() {
            true => Vec::new(),
            false => Report::new(snapshot, self.latency_server(), width - 4, *theme)
                .results("Latency")
                .view(),
        };
        if results.is_empty() {
            return self.stage_view(snapshot, run, width, height);
        }
        let mut title = "Results".to_owned();
        if let Some(id) = self.latency_server().filter(|_| self.several()) {
            title = format!("{title} · latency to {}", server_name(snapshot, id));
        }
        let widest = results.iter().map(Line::width).max().unwrap_or(0);
        let results_width = widest.max(title.width() + 2) + 4;
        let bottom = if width >= TWO_COLUMN_MIN && width.saturating_sub(1 + results_width) >= 30 {
            let fields = self.test_fields(snapshot, run, width - 1 - results_width - 4);
            let height = results.len().max(fields.len()) + 2;
            let fields = panel("Test", fields, width - 1 - results_width, height, theme);
            join(panel(&title, results, results_width, height, theme), fields, true)
        } else {
            panel(&title, results, width, 0, theme)
        };
        match height.saturating_sub(bottom.len()) {
            timeline if timeline >= 8 => join(self.timeline_panel(snapshot, run, width, timeline), bottom, false),
            _ => bottom,
        }
    }

    /// Go's stageView: the Test panel beside or above the timeline.
    fn stage_view(&self, snapshot: &Snapshot, run: &Run, width: usize, height: usize) -> Text {
        let theme = &self.theme;
        let (mut left, mut right, side) = columns(width);
        if side {
            (left, right) = (width * 2 / 5, width - 1 - width * 2 / 5);
        }
        let live = snapshot.phase.live();
        let test = self.test_view(snapshot, run, left - 4, !side);
        let test_height = test.len() + 2;
        let mut live_height = height;
        if !side {
            live_height = live_height.saturating_sub(test_height);
        }
        if side && (live || live_height >= 9) {
            let live_height = live_height.max(test_height).max(9);
            let timeline = self.timeline_panel(snapshot, run, right, live_height);
            return join(panel("Test", test, left, live_height, theme), timeline, true);
        }
        if live || live_height >= 9 {
            let timeline = self.timeline_panel(snapshot, run, right, live_height.max(7));
            return join(panel("Test", test, left, 0, theme), timeline, false);
        }
        panel("Test", self.test_view(snapshot, run, width - 4, !side), width, 0, theme)
    }

    /// Go's timelinePanel.
    fn timeline_panel(&self, snapshot: &Snapshot, run: &Run, width: usize, height: usize) -> Text {
        let mut title = "Timeline".to_owned();
        if snapshot.phase.live() {
            title = format!("{title} · {}", self.status_label());
        }
        let live = self.live_view(snapshot, run, width - 4, height - 2);
        panel(&title, live, width, height, &self.theme)
    }

    /// Go's testView: the run's settings over the stage track, or the track alone.
    fn test_view(&self, snapshot: &Snapshot, run: &Run, width: usize, compact: bool) -> Text {
        let track = self.stage_track(snapshot, run, width);
        if compact {
            return track;
        }
        let mut out = self.test_fields(snapshot, run, width);
        out.push(Line::default());
        out.extend(track);
        out
    }

    /// Go's testFields: the run's servers and paths, then its stream and timing settings.
    fn test_fields(&self, snapshot: &Snapshot, run: &Run, width: usize) -> Text {
        let theme = &self.theme;
        let label = |name: &str| span(format!("{name:<11}"), theme.text);
        if !snapshot.started() {
            let mut line = vec![label("Servers")];
            if snapshot.phase.live() {
                line.extend([self.spinner(), span(" Checking paths…", theme.muted)]);
            } else {
                line.push(span(MISSING, theme.muted));
            }
            return vec![Line::from(line)];
        }
        let servers = run_servers(snapshot);
        let mut throughputs: Vec<String> = Vec::new();
        for server in &servers {
            let path = server.throughput.as_ref().map(words::throughput_path);
            if let Some(path) = path.filter(|path| !throughputs.contains(path)) {
                throughputs.push(path);
            }
        }
        let shown = self.latency_server();
        let latency = servers.iter().find(|server| Some(server.id.as_str()) == shown);
        let latency = latency.and_then(|server| server.latency.as_ref().map(words::latency_path));
        let names: Vec<_> = servers.iter().map(|server| server.name.as_str()).collect();
        let mut names = names.join(", ");
        let target = servers.iter().rev().find_map(|server| server.throughput.as_ref());
        let mut streams = words::streams(&run.config, target);
        if servers.len() > 1 {
            names.push_str(" (all servers)");
            streams = format!("per server · {streams}");
        }
        let timing = format!(
            "warmup {} · latency cadence {} · loaded cadence {}",
            words::setting(run.config.warmup),
            words::cadence(run.config.ping_interval),
            words::cadence(run.config.loaded_ping_interval)
        );
        let mut lines = Vec::new();
        for (name, value) in [
            ("Servers", names),
            ("Throughput", throughputs.join(" / ")),
            ("Latency", latency.unwrap_or_else(|| MISSING.to_owned())),
            ("Streams", streams),
            ("Timing", timing),
        ] {
            let parts: Vec<_> = value.split(" · ").map(str::to_owned).collect();
            let parts = wrap_parts(&parts, width.saturating_sub(11).max(12));
            for (index, text) in parts.into_iter().enumerate() {
                let name = if index == 0 { name } else { "" };
                lines.push(Line::from(vec![label(name), span(text, theme.value)]));
            }
        }
        lines
    }

    /// Go's stageTrack: each planned stage's wait, progress or outcome.
    pub(super) fn stage_track(&self, snapshot: &Snapshot, run: &Run, width: usize) -> Text {
        let theme = &self.theme;
        let report = Report::new(snapshot, self.latency_server(), width, *theme);
        let bar_width = width.saturating_sub(34).clamp(6, 30);
        let live = snapshot.phase.live();
        let mut lines = Vec::new();
        for stage in &run.config.stages {
            let hue = theme.stage(*stage);
            let duration = run.config.duration(*stage);
            let mut line = vec![span(format!("{:<14}", stage.name()), hue)];
            let result = snapshot.results.iter().rfind(|result| result.stage == *stage);
            let current = snapshot.stage == Some(*stage);
            match result.map(|result| snapshot.stage_status(result)) {
                Some(StageStatus::Complete) => {
                    let (headline, planned) = (report.headline(*stage), span(words::setting(duration), theme.muted));
                    line.push(span("✓ ", theme.ok));
                    line.extend(if headline.is_empty() { vec![planned] } else { headline });
                }
                Some(StageStatus::Partial) => {
                    let headline = report.headline(*stage);
                    let gap = (!headline.is_empty()).then(|| Span::raw(" "));
                    line.push(span("! ", theme.warn));
                    line.extend(headline.into_iter().chain(gap).chain([span("Partial", theme.muted)]));
                }
                Some(StageStatus::Stopped) => {
                    line.extend([span("○ ", theme.muted), span(StageStatus::Stopped.label(), theme.muted)])
                }
                Some(status) => line.extend([span("✗ ", theme.err), span(status.label(), theme.muted)]),
                None if current && live => match snapshot.phase {
                    Phase::Warmup => line.extend([
                        self.spinner(),
                        span(" warmup ", theme.muted),
                        span(words::clock(run.elapsed()), theme.value),
                    ]),
                    Phase::Measuring => {
                        let (elapsed, total) = (run.elapsed(), duration.as_secs_f64());
                        line.extend(bar(hue, elapsed.as_secs_f64(), total, bar_width, theme));
                        line.extend([
                            Span::raw("  "),
                            span(words::clock(elapsed.min(duration)), theme.value),
                            span(format!(" / {}", words::setting(duration)), theme.muted),
                        ]);
                    }
                    _ => line.extend([self.spinner(), span(" checking paths", theme.muted)]),
                },
                None if current => {
                    line.extend([span("○ ", theme.muted), span(StageStatus::Stopped.label(), theme.muted)])
                }
                None if !live => line.push(span(format!("{MISSING} {}", StageStatus::Skipped.label()), theme.muted)),
                None => line.push(span(format!("○ {}", words::setting(duration)), theme.muted)),
            }
            lines.push(Line::from(line));
        }
        lines
    }

    /// Go's liveView: the readings over the charts of the current stage's directions.
    pub(super) fn live_view(&self, snapshot: &Snapshot, run: &Run, width: usize, height: usize) -> Text {
        let theme = &self.theme;
        let live = snapshot.phase.live();
        let stage = snapshot.stage.filter(|stage| run.config.stages.contains(stage));
        if stage.is_none() && !live {
            return vec![line(MISSING, theme.muted)];
        }
        let Some(stage) = stage.filter(|_| !live || snapshot.phase != Phase::Preparing) else {
            return vec![Line::from(vec![self.spinner(), span(" Checking paths…", theme.muted)])];
        };
        let mut directions = Vec::new();
        for (index, (moves, trace)) in [stage.downloads(), stage.uploads()]
            .into_iter()
            .zip(&run.traces)
            .enumerate()
        {
            if if live { moves } else { !trace.points.is_empty() } {
                directions.push((index, trace));
            }
        }
        let loaded = !directions.is_empty() && run.config.loaded_latency;
        let series: Vec<_> = directions
            .iter()
            .flat_map(|(index, trace)| stage_series(&trace.points, &run.marks, theme, *index == 1))
            .collect();
        let mut out = Vec::new();
        if live {
            out.extend(self.readings(snapshot, run, stage, width));
        }
        if stage == Stage::Bidirectional || !live && run.marks.iter().any(|(_, stage)| *stage == Stage::Bidirectional) {
            out.push(line("↓ solid · ↑ dashed", theme.muted));
        }
        if self.several() {
            let name = self.latency_server().map_or("", |id| server_name(snapshot, id));
            out.push(line(format!("Latency to {name} · l switches server"), theme.muted));
        }
        let (chart_height, time) = (height.saturating_sub(out.len()), run.span());
        let rtt = self.latency_server().and_then(|id| run.rtt.get(id));
        let rtt = stage_series(rtt.map_or(&[], |trace| &trace.points), &run.marks, theme, false);
        let rates = |height| chart(&series, &run.marks, RATE_AXIS, time, width, height, theme);
        let rtts = |marks: &[(f64, Stage)], height| chart(&rtt, marks, MS_AXIS, time, width, height, theme);
        if chart_height < 5 {
        } else if directions.is_empty() {
            out.extend(rtts(&run.marks, chart_height));
        } else if loaded && chart_height >= 12 {
            out.extend(rates(chart_height - 5));
            out.extend(rtts(&run.marks, 5));
        } else {
            out.extend(rates(chart_height));
        }
        out
    }

    /// Go's readings: each direction's rate, then the latency the stage measures.
    fn readings(&self, snapshot: &Snapshot, run: &Run, stage: Stage, width: usize) -> Text {
        let theme = &self.theme;
        let mut readings = Vec::new();
        let latest = &snapshot.latest;
        let sampled = latest.sample_count > 0 && snapshot.phase == Phase::Measuring;
        for index in directions(stage) {
            let value = match (sampled, [latest.down_bps, latest.up_bps][index], run.shown[index]) {
                (false, ..) => span(MISSING, theme.muted),
                (true, None, _) => span(format!("{MISSING} window restarting"), theme.muted),
                (true, Some(rate), shown) => span(format::rate(shown.unwrap_or(rate) / 8.0), theme.value),
            };
            readings.push(vec![span(format!("{} ", ARROWS[index]), theme.stage(stage)), value]);
        }
        let transfers = stage.downloads() || stage.uploads();
        if !transfers || run.config.loaded_latency {
            let label = if transfers { "Loaded latency " } else { "Idle latency " };
            let shown = self.latency_server();
            let value = match shown.and_then(|id| run.latest.get(id)) {
                Some(ms) => span(format!("{} ms", format::latency_ms(*ms)), theme.value),
                None => span(MISSING, theme.muted),
            };
            let mut reading = vec![span(label, theme.text), value];
            let mut hosts = snapshot.server_latencies.iter();
            let host = hosts.find(|host| Some(host.id.as_str()) == shown);
            if let Some(streak) = host.map(|host| host.timeouts).filter(|streak| *streak > 0) {
                let style = if streak >= 3 { theme.err } else { theme.warn };
                reading.push(span(format!("  probe timeout ×{streak}"), style));
            }
            readings.push(reading);
        }
        let mut lines = Vec::new();
        let mut current = Line::default();
        for reading in readings {
            let reading = Line::from(reading);
            if !current.spans.is_empty() {
                if current.width() + 3 + reading.width() > width {
                    lines.push(std::mem::take(&mut current));
                } else {
                    current.spans.push(Span::raw("   "));
                }
            }
            current.spans.extend(reading.spans);
        }
        lines.push(current);
        lines
    }
}

