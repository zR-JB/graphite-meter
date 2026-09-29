//! Go's run view (view.go and ui.go): the stage track, the timeline's charts over the run's time,
//! and the results with the test's settings.
use super::{
    Ui,
    view::{TWO_COLUMN_MIN, columns, join, panel},
};
use crate::{
    config::Config,
    model::{Phase, Snapshot, Stage, StageStatus},
    report::{Report, Text, fit, line, plain, run_servers, server_name, span, wrap_parts},
    theme::Theme,
    vocabulary::{self as words, MISSING, compact_stage},
};
use graphite_meter_core::format;
use ratatui::{
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
    points: Vec<(f64, f64, usize)>,
    step: f64,
}

impl Trace {
    pub(super) fn add(&mut self, at: f64, value: f64) {
        self.step = self.step.max(TRACE_STEP);
        if let Some(last) = self.points.last_mut()
            && at - last.0 < self.step
            && !value.is_nan()
            && !last.1.is_nan()
        {
            last.2 += 1;
            last.1 += (value - last.1) / last.2 as f64;
            return;
        }
        if self.points.len() == HISTORY_POINTS {
            // Go's coarsen: pairs merge, and a gap in either keeps the pair a gap.
            self.points = self
                .points
                .chunks(2)
                .map(|pair| {
                    let mut point = pair[0];
                    if let Some(next) = pair.get(1) {
                        if next.1.is_nan() {
                            point.1 = next.1;
                        } else if !point.1.is_nan() {
                            point.1 = (point.1 * point.2 as f64 + next.1 * next.2 as f64) / (point.2 + next.2) as f64;
                            point.2 += next.2;
                        }
                    }
                    point
                })
                .collect();
            self.step *= 2.0;
        }
        self.points.push((at, value, 1));
    }
}

/// What Go's runState keeps for the view: the run's clock, stage marks, the traces its charts
/// draw, the latest round trips and the smoothed rates.
#[derive(Clone, Default)]
pub(super) struct Run {
    pub config: Config,
    started: Option<Instant>,
    ended: Option<Instant>,
    marks: Vec<(f64, Stage)>,
    down: Trace,
    up: Trace,
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
        Self {
            config,
            started: Some(Instant::now()),
            ..Self::default()
        }
    }

    fn clock(&self, at: Instant) -> f64 {
        self.started
            .map_or(0.0, |started| at.saturating_duration_since(started).as_secs_f64())
    }

    /// Go's apply: stage events set the marks and clear the stage's readings; samples extend the traces.
    pub(super) fn observe(&mut self, snapshot: &Snapshot) {
        let now = Instant::now();
        let at = self.clock(now);
        let step = (snapshot.stage, snapshot.phase);
        if self.step != Some(step) {
            if snapshot.phase == Phase::Preparing {
                self.latest.clear();
                self.shown = [None; 2];
            }
            if let (Some(stage), Phase::Measuring) = step {
                self.marks.push((at, stage));
                for trace in [&mut self.down, &mut self.up].into_iter().chain(self.rtt.values_mut()) {
                    trace.add(at, f64::NAN);
                }
            }
            (self.step, self.since, self.sample) = (Some(step), Some(now), None);
        }
        let latest = &snapshot.latest;
        if snapshot.phase == Phase::Measuring && latest.sample_count > 0 && self.sample != Some(latest.elapsed) {
            self.sample = Some(latest.elapsed);
            let stage = snapshot.stage.unwrap_or_default();
            for (index, (trace, rate, moves)) in [
                (&mut self.down, latest.down_bps, stage.downloads()),
                (&mut self.up, latest.up_bps, stage.uploads()),
            ]
            .into_iter()
            .enumerate()
            {
                if moves {
                    trace.add(at, rate.map_or(f64::NAN, |bits| bits / 8.0));
                    if rate.is_none() || self.shown[index].is_none() {
                        self.shown[index] = rate;
                    }
                }
            }
            for host in &snapshot.server_latencies {
                let Some(ms) = host.latest_ms else { continue };
                self.latest.insert(host.id.clone(), ms);
                self.rtt.entry(host.id.clone()).or_default().add(at, ms * 1e6);
            }
        }
        if !snapshot.phase.live() {
            self.ended.get_or_insert(now);
        }
    }

    /// Go's frame tick: the shown rates ease towards the latest.
    pub(super) fn ease(&mut self, snapshot: &Snapshot) {
        let targets = [snapshot.latest.down_bps, snapshot.latest.up_bps];
        for (shown, target) in self.shown.iter_mut().zip(targets) {
            *shown = target.map(|target| shown.map_or(target, |value| value + (target - value) * 0.35));
        }
    }

    /// How long the current stage phase has run, as Go's now minus the stage's since.
    fn elapsed(&self) -> Duration {
        self.since.map_or(Duration::ZERO, |since| since.elapsed())
    }

    /// Go's span: the run's time so far, or until it ended.
    fn span(&self) -> f64 {
        self.clock(self.ended.unwrap_or_else(Instant::now))
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
        let tier = (1..units.len())
            .take_while(|tier| bits >= 1000f64.powi(*tier as i32))
            .count();
        format!("{} {}", round_label(bits / 1000f64.powi(tier as i32)), units[tier])
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
    [1.0, 2.0, 2.5, 5.0]
        .into_iter()
        .map(|step| step * decade)
        .find(|ceiling| *ceiling >= value)
        .unwrap_or(10.0 * decade)
}

/// A chart line: its stage's style and its points.
type Series<'a> = (Style, &'a [(f64, f64, usize)]);

/// Go's stageSeries: the points after each mark, in its stage's style.
fn stage_series<'a>(mut points: &'a [(f64, f64, usize)], marks: &[(f64, Stage)], theme: &Theme) -> Vec<Series<'a>> {
    let mut out = vec![(Style::new(), &points[..0]); marks.len()];
    for (index, (at, stage)) in marks.iter().enumerate().rev() {
        let start = points.partition_point(|point| point.0 < *at);
        out[index] = (theme.stage(*stage), &points[start..]);
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
    let peak = lines
        .iter()
        .flat_map(|(_, points)| points.iter())
        .filter(|point| !point.1.is_nan())
        .fold(0.0_f64, |peak, point| peak.max(point.1));
    let top = nice_ceil(peak * axis.scale * 1.05) / axis.scale;
    let (dot_width, dot_height) = (cols * 2, rows * 4);
    let (mut dots, mut owner) = (vec![0u8; cols * rows], vec![0usize; cols * rows]);
    let mut set = |x: isize, y: isize, series: usize| {
        let (x, y) = (x as usize, y as usize);
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
    for (index, (_, points)) in lines.iter().enumerate() {
        // Each dot column's weighted sum and count, where None is a gap that breaks the line.
        let mut columns: Vec<Option<(isize, f64, usize)>> = Vec::new();
        for point in points.iter() {
            let x = (((point.0 - t0) / (t1 - t0) * dot_width as f64) as isize).clamp(0, dot_width as isize - 1);
            match columns.last_mut() {
                _ if point.1.is_nan() => columns.push(None),
                Some(Some((column, sum, count))) if *column == x => {
                    (*sum, *count) = (*sum + point.1 * point.2 as f64, *count + point.2);
                }
                _ => columns.push(Some((x, point.1 * point.2 as f64, point.2))),
            }
        }
        let mut last = None;
        for column in columns {
            let Some((x, sum, count)) = column else {
                last = None;
                continue;
            };
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
                spans.push(Span::raw(" ".repeat(end - first)));
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
    out.push(Line::from(vec![
        Span::raw(" ".repeat(CHART_AXIS - 1)),
        span(format!("└{}", ruler.into_iter().collect::<String>()), theme.border),
    ]));
    let mut last = vec![Span::raw(" ".repeat(CHART_AXIS))];
    last.extend(labels);
    last.extend([
        Span::raw(" ".repeat(end_at.saturating_sub(written))),
        span(end, theme.muted),
    ]);
    out.push(Line::from(last));
    out
}

/// Go's bar: whole cells and an eighth, over the rest in shade.
fn bar(fill: Style, value: f64, scale: f64, width: usize, theme: &Theme) -> Vec<Span<'static>> {
    let cells = if scale > 0.0 {
        (value / scale * width as f64).clamp(0.0, width as f64)
    } else {
        0.0
    };
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
        let mut bottom = panel(&title, results.clone(), width, 0, theme);
        let widest = results.iter().map(Line::width).max().unwrap_or(0);
        let results_width = widest.max(title.width() + 2) + 4;
        if width >= TWO_COLUMN_MIN && width.saturating_sub(1 + results_width) >= 30 {
            let fields = self.test_fields(snapshot, run, width - 1 - results_width - 4);
            let height = results.len().max(fields.len()) + 2;
            bottom = join(
                panel(&title, results, results_width, height, theme),
                panel("Test", fields, width - 1 - results_width, height, theme),
                true,
            );
        }
        match height.saturating_sub(bottom.len()) {
            timeline if timeline >= 8 => [self.timeline_panel(snapshot, run, width, timeline), bottom].concat(),
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
            return join(
                panel("Test", test, left, live_height, theme),
                self.timeline_panel(snapshot, run, right, live_height),
                true,
            );
        }
        if live || live_height >= 9 {
            return [
                panel("Test", test, left, 0, theme),
                self.timeline_panel(snapshot, run, right, live_height.max(7)),
            ]
            .concat();
        }
        panel("Test", self.test_view(snapshot, run, width - 4, !side), width, 0, theme)
    }

    /// Go's timelinePanel.
    fn timeline_panel(&self, snapshot: &Snapshot, run: &Run, width: usize, height: usize) -> Text {
        let mut title = "Timeline".to_owned();
        if snapshot.phase.live() {
            title = format!("{title} · {}", self.status_label());
        }
        panel(
            &title,
            self.live_view(snapshot, run, width - 4, height - 2),
            width,
            height,
            &self.theme,
        )
    }

    /// Go's testView: the run's settings over the stage track, or the track alone.
    fn test_view(&self, snapshot: &Snapshot, run: &Run, width: usize, compact: bool) -> Text {
        let track = self.stage_track(snapshot, run, width);
        if compact {
            return track;
        }
        [self.test_fields(snapshot, run, width), vec![Line::default()], track].concat()
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
        let mut throughputs = Vec::new();
        for path in servers
            .iter()
            .filter_map(|server| server.throughput.as_ref().map(words::throughput_path))
        {
            if !throughputs.contains(&path) {
                throughputs.push(path);
            }
        }
        let latency = servers
            .iter()
            .find(|server| Some(server.id.as_str()) == self.latency_server())
            .and_then(|server| server.latency.as_ref().map(words::latency_path));
        let mut names = servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
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
            for (index, text) in wrap_parts(&parts, width.saturating_sub(11).max(12))
                .into_iter()
                .enumerate()
            {
                lines.push(Line::from(vec![
                    label(if index == 0 { name } else { "" }),
                    span(text, theme.value),
                ]));
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
                    line.extend([span("✗ ", theme.err), span(StageStatus::Stopped.label(), theme.muted)])
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
        for (moves, trace) in [(stage.downloads(), &run.down), (stage.uploads(), &run.up)] {
            if if live { moves } else { !trace.points.is_empty() } {
                directions.push(trace);
            }
        }
        let loaded = !directions.is_empty() && run.config.loaded_latency;
        let series: Vec<_> = directions
            .iter()
            .flat_map(|trace| stage_series(&trace.points, &run.marks, theme))
            .collect();
        let mut out = Vec::new();
        if live {
            out.push(self.readings(snapshot, run, stage));
        }
        if self.several() {
            let name = self.latency_server().map_or("", |id| server_name(snapshot, id));
            out.push(line(format!("Latency to {name} · l switches server"), theme.muted));
        }
        let (chart_height, time) = (height.saturating_sub(out.len()), run.span());
        let rtt = self.latency_server().and_then(|id| run.rtt.get(id));
        let rtt = stage_series(rtt.map_or(&[], |trace| &trace.points), &run.marks, theme);
        let rates = |height| chart(&series, &run.marks, RATE_AXIS, time, width, height, theme);
        let rtts = |marks: &[(f64, Stage)], height| chart(&rtt, marks, MS_AXIS, time, width, height, theme);
        if chart_height < 5 {
        } else if directions.is_empty() {
            out.extend(rtts(&run.marks, chart_height));
        } else if loaded && chart_height >= 12 {
            out.extend([rates(chart_height - 5), rtts(&[], 5)].concat());
        } else {
            out.extend(rates(chart_height));
        }
        out
    }

    /// Go's readings: each direction's rate, then the latency the stage measures.
    fn readings(&self, snapshot: &Snapshot, run: &Run, stage: Stage) -> Line<'static> {
        let theme = &self.theme;
        let mut readings = Vec::new();
        let latest = &snapshot.latest;
        for (moves, arrow, rate, shown) in [
            (stage.downloads(), "↓", latest.down_bps, run.shown[0]),
            (stage.uploads(), "↑", latest.up_bps, run.shown[1]),
        ] {
            if !moves {
                continue;
            }
            let value = match (
                latest.sample_count > 0 && snapshot.phase == Phase::Measuring,
                rate,
                shown,
            ) {
                (false, ..) => span(MISSING, theme.muted),
                (true, None, _) => span(format!("{MISSING} window restarting"), theme.muted),
                (true, Some(rate), shown) => span(format::rate(shown.unwrap_or(rate) / 8.0), theme.value),
            };
            readings.push(vec![span(format!("{arrow} "), theme.text), value]);
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
            let host = snapshot
                .server_latencies
                .iter()
                .find(|host| Some(host.id.as_str()) == shown);
            if let Some(streak) = host.map(|host| host.timeouts).filter(|streak| *streak > 0) {
                let style = if streak >= 3 { theme.err } else { theme.warn };
                reading.push(span(format!("  probe timeout ×{streak}"), style));
            }
            readings.push(reading);
        }
        Line::from(readings.join(&Span::raw("   ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go's TestChartJoinsSamplesAndBreaksOnlyAtGaps, and an empty chart claims no scale.
    #[test]
    fn charts_join_samples_and_break_only_at_gaps() {
        let mut trace = Trace::default();
        for index in 0..200 {
            let value = match index {
                100 => f64::NAN,
                101..130 => continue,
                _ => 1e6 + 9e5 * (f64::from(index) / 3.0).sin(),
            };
            trace.add(f64::from(index) * 0.1, value);
        }
        let theme = Theme::new(crate::theme::Profile::TrueColor, true);
        let marks = [(0.0, Stage::Latency), (4.0, Stage::Download), (19.5, Stage::Upload)];
        for width in [36, 76, 116] {
            let series = [(theme.stage(Stage::Download), &trace.points[..])];
            let lines = chart(&series, &marks, RATE_AXIS, 20.0, width, 12, &theme);
            let mut inked = vec![false; 2 * width];
            for line in &lines[..lines.len() - 2] {
                for (x, character) in plain(line).chars().skip(CHART_AXIS).enumerate() {
                    let bits = u32::from(character).wrapping_sub(0x2800);
                    if (1..=0xff).contains(&bits) {
                        inked[2 * x] |= bits & 0x47 != 0;
                        inked[2 * x + 1] |= bits & 0xb8 != 0;
                    }
                }
            }
            let dot = |at: f64| (at / 20.0 * (2 * (width - CHART_AXIS)) as f64) as usize;
            for (x, inked) in inked.iter().enumerate().take(dot(19.9) + 1) {
                let gap = x > dot(9.9) && x < dot(13.0);
                assert_ne!(*inked, gap, "width {width}: dot column {x}");
            }
            assert!(lines.iter().all(|line| line.width() <= width), "width {width}");
        }
        let empty = chart(&[], &[], RATE_AXIS, 1.0, 40, 6, &theme);
        assert!(
            !empty.iter().any(|line| plain(line).contains("bit/s")),
            "an empty chart claims a scale"
        );
    }
}
