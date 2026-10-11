//! The console, the one screen before, during and after a run, as the browser's: the dial beside the latency lanes,
//! the run key over the stage chips, and a card per stage across the bottom (`docs/DESIGN.md`, Terminal).
use super::{
    App, Target, chart,
    dial::{self, Arc, Dial},
    paint::Canvas,
    run::State,
    theme::mix,
};
use crate::{
    events::{Point, Series},
    measure::{format, latency::Summary, wire},
    model::{Outcome, ServerFailure, Stage, StageResult, StageStatus},
    report::vocabulary::{self as words, MISSING},
    text::{self, Line, Profile, Style},
};
use graphite_meter_proto::reason::FailureReason;
use ratatui_core::layout::Rect;
use std::time::Duration;

/// From this size the dial stands beside the lanes over the cards; below it the console stacks without the ring.
const WIDE: (u16, u16) = (110, 32);
/// The browser's chart steps (`client/src/lib/presentation/scales.ts`) and its latency ladder.
const CHART_STEPS: [f64; 10] = [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0];
const LADDER: [f64; 3] = [1.0, 2.0, 4.0];
/// The run key's and the chips' widest.
const CONTROLS: u16 = 64;
/// The browser's dial transfer curve (`client/src/lib/components/gaugeScale.ts`): equal sweeps for each knot.
const KNOTS: [f64; 9] = [0.0, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0];

/// Where `value` sits on a dial reaching `scale`, from 0 to 1.
pub fn gauge_fraction(value: f64, scale: f64) -> f64 {
    let v = (value / scale).clamp(0.0, 1.0);
    if v.is_nan() {
        return 0.0;
    }
    let spans = (KNOTS.len() - 1) as f64;
    let i = (1..KNOTS.len()).find(|&i| v <= KNOTS[i]).unwrap_or(KNOTS.len() - 1);
    (i as f64 - 1.0 + (v - KNOTS[i - 1]) / (KNOTS[i] - KNOTS[i - 1])) / spans
}

/// The value at `fraction` of a dial reaching `scale`.
pub fn gauge_value(fraction: f64, scale: f64) -> f64 {
    let at = fraction.clamp(0.0, 1.0) * (KNOTS.len() - 1) as f64;
    let i = (at as usize).min(KNOTS.len() - 2);
    scale * (KNOTS[i] + (at - i as f64) * (KNOTS[i + 1] - KNOTS[i]))
}

/// The browser's axis ceiling (`client/src/lib/presentation/scales.ts`): the first step at or above `v`.
pub fn ceil_step(v: f64, steps: &[f64]) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return steps[0];
    }
    let decade = 10_f64.powf(v.log10().floor());
    steps
        .iter()
        .map(|step| step * decade)
        .find(|&step| step >= v)
        .unwrap_or(10.0 * decade)
}

/// The browser's tick label: bounded precision, no trailing zeros.
pub fn gauge_tick(v: f64) -> String {
    if v == 0.0 || v.is_nan() {
        return "0".into();
    }
    let places = (2 - v.abs().log10().floor() as i32).clamp(0, 6) as usize;
    let text = format!("{v:.places$}");
    match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.').into(),
        false => text,
    }
}

pub fn icon(stage: Stage) -> &'static str {
    ["≈", "↓", "↑", "↕"][stage as usize]
}

/// The browser's dial scale for a combined rate in bytes per second: its 1-2-5 step above it with 3% headroom, and from
/// a megabit up never under 1 Gbit/s, where most connections fit.
pub fn gauge_ceiling(bytes_per_sec: f64) -> f64 {
    let bits = bytes_per_sec * 8.0 * 1.03;
    let top = ceil_step(bits, &[1.0, 2.0, 5.0]);
    let top = if bits >= 1e6 { top.max(1e9) } else { top };
    top / 8.0
}

/// The page's rate unit, the dial's, from the highest rate so far; before any, the browser's 100 Mbit/s reference.
#[derive(Debug, Clone, Copy)]
pub struct Unit {
    divisor: f64,
    pub name: &'static str,
}

impl Unit {
    pub fn of(peak: f64) -> Self {
        let peak = if peak > 0.0 { peak } else { 12.5e6 };
        let (shown, name) = format::tier(peak * 8.0, 1.2);
        Self { divisor: peak * 8.0 / shown, name }
    }

    /// `bytes_per_sec` as a figure in this unit.
    pub fn figure(self, bytes_per_sec: f64) -> String {
        format::speed(bytes_per_sec * 8.0 / self.divisor)
    }
}

/// The cards' tops: the rate charts' in bytes per second, and the idle and loaded reply tracks' in milliseconds.
#[derive(Debug, Clone, Copy)]
struct Scales {
    rate: f64,
    idle: f64,
    loaded: f64,
}

/// A latency lane: a population's statistics, its latest reply while it is measured, and whether it is.
pub(super) struct Lane {
    pub stage: Stage,
    pub summary: Option<Summary>,
    pub latest: Option<Duration>,
    pub live: bool,
}

impl App {
    /// The console in `area`: wide, the dial beside the lanes and controls over the cards; narrow, stacked.
    pub(super) fn console(&self, canvas: &mut Canvas, area: Rect) {
        let lanes = self.lanes();
        if area.width >= WIDE.0 && area.height >= WIDE.1 {
            let top = (area.height * 11 / 20).clamp(18, 24);
            let dial = Rect::new(area.x + 1, area.y + 1, (area.width * 3 / 10).clamp(46, 66), top);
            dial::draw(canvas, dial, &self.palette, &self.dial());
            let side = Rect::new(dial.right() + 3, dial.y, area.right().saturating_sub(dial.right() + 5), top);
            let mut controls = side;
            if !lanes.is_empty() {
                let pitch = if top >= 22 { 2 } else { 1 };
                let height = 4 + pitch * lanes.len() as u16;
                self.lanes_area(canvas, Rect::new(side.x, side.y, side.width, height), &lanes, pitch);
                controls = Rect::new(side.x, side.y + height + 1, side.width, side.height.saturating_sub(height + 1));
            }
            let y = controls.y + controls.height.saturating_sub(6) / 2;
            self.controls(canvas, Rect::new(controls.x, y, controls.width, 6));
            let cards = Rect::new(area.x + 1, dial.bottom() + 1, area.width - 2, area.bottom() - dial.bottom() - 1);
            self.cards(canvas, cards);
            return;
        }
        // Stacked: the readout, the controls, the cards, and the lanes when room is left; the shortest area drops the
        // gap under the readout.
        let readout = Rect::new(area.x, area.y, area.width, 8);
        dial::readout(canvas, readout, &self.palette, &self.dial());
        let width = area.width.saturating_sub(4).min(CONTROLS);
        let gap = area.height.saturating_sub(14).min(1);
        let controls = Rect::new(area.x + (area.width - width) / 2, readout.bottom() + gap, width, 6);
        self.controls(canvas, controls);
        let rest = area.bottom().saturating_sub(controls.bottom() + 1);
        let lanes_height = 4 + lanes.len() as u16;
        let lanes_room = !lanes.is_empty() && rest > 8 + lanes_height;
        let cards_height = if lanes_room { rest - lanes_height - 1 } else { rest };
        let cards = Rect::new(area.x + 1, controls.bottom() + 1, area.width - 2, cards_height);
        self.cards(canvas, cards);
        if lanes_room {
            let lanes_area = Rect::new(area.x + 1, cards.bottom() + 1, area.width - 2, lanes_height);
            self.lanes_area(canvas, lanes_area, &lanes, 1);
        }
    }

    /// The run key over the stage chips, as wide as each other.
    fn controls(&self, canvas: &mut Canvas, area: Rect) {
        let width = area.width.min(CONTROLS);
        let x = area.x + (area.width - width) / 2;
        self.run_key(canvas, Rect::new(x, area.y, width, 3));
        self.chips(canvas, Rect::new(x, area.y + 4, width, 2));
    }

    /// Start test, Stop test while a run goes, Run again after one; a key that cannot start says why on it.
    fn run_key(&self, canvas: &mut Canvas, area: Rect) {
        let palette = &self.palette;
        let hovered = self.hover == Some(Target::Key);
        let (label, note, cap, plate, faint) = match self.blocked() {
            _ if self.running() => {
                let plate = if hovered { palette.selected } else { palette.raised };
                let faint = Style { bg: Some(plate), ..palette.muted };
                (
                    "■  Stop test".to_owned(),
                    String::new(),
                    "esc",
                    Style { bg: Some(plate), ..palette.value },
                    faint,
                )
            }
            Some(reason) => ("Start test".into(), reason, "", palette.plate_off, palette.plate_off),
            None => {
                let label = if self.view.run.is_some() { "Run again" } else { "Start test" };
                let mut plate = palette.plate;
                if hovered {
                    plate.bg = plate.bg.map(|ink| mix(palette.canvas(), ink, 0.12));
                }
                let faint = Style { bg: plate.bg, ..palette.plate_note };
                (label.into(), format!("~{}", self.plan_time()), "enter", plate.bold(), faint)
            }
        };
        canvas.plate(area, plate);
        let gap = if note.is_empty() { "" } else { "   " };
        let text = Line::styled(label, plate).and(gap, plate).and(note, faint);
        canvas.center(area.x, area.y + 1, area.width, &text);
        if !cap.is_empty() && area.width >= 30 {
            canvas.right(area.x, area.y + 1, area.width - 2, &Line::styled(cap, faint));
        }
        canvas.hit(area, Target::Key);
    }

    /// Why the key cannot start a test now, if it cannot.
    pub(super) fn blocked(&self) -> Option<String> {
        match () {
            _ if self.running() => None,
            _ if self.config.url.is_none() => Some("needs a server".into()),
            _ => self.config.validate().err().map(|_| "turn on a stage".into()),
        }
    }

    /// A chip per stage: its bar fills as the stage runs, under its mark, name and time or state.
    fn chips(&self, canvas: &mut Canvas, area: Rect) {
        let stages = self.chip_stages();
        let count = stages.len() as u16;
        let width = area.width.saturating_sub(count.saturating_sub(1)) / count.max(1);
        for (index, &stage) in stages.iter().enumerate() {
            let x = area.x + index as u16 * (width + 1);
            let last = index as u16 + 1 == count;
            self.chip(canvas, Rect::new(x, area.y, if last { area.right() - x } else { width }, 2), stage);
        }
    }

    /// The stages the chips show: the default three, and bidirectional while it is on or ran, in order.
    fn chip_stages(&self) -> Vec<Stage> {
        let planned = self.plan();
        let stages = [Stage::Latency, Stage::Download, Stage::Upload, Stage::Bidirectional].into_iter();
        let shown = |stage: &Stage| *stage != Stage::Bidirectional || planned.iter().any(|(own, _)| own == stage);
        stages.filter(shown).collect()
    }

    fn chip(&self, canvas: &mut Canvas, area: Rect, stage: Stage) {
        let palette = &self.palette;
        let on = self.stage_on(stage);
        // Between runs a chip that is off says so, whatever the last run did.
        let state = if on || self.running() { self.stage_state(stage) } else { State::Pending };
        let planned = match self.running() {
            true => self
                .plan()
                .into_iter()
                .find(|(own, _)| *own == stage)
                .map(|(_, duration)| duration),
            false => on.then(|| self.config.duration(stage)),
        };
        let hovered = self.hover == Some(Target::Chip(stage)) && !self.running();
        let tile = match state {
            State::Measuring | State::Warmup => palette.wash(stage, 0.16, palette.canvas()),
            _ if hovered => palette.hover,
            _ => palette.surface,
        };
        canvas.fill(area, tile);
        let hue = palette.trace(stage);
        let track = mix(palette.border.fg.unwrap_or(hue), tile, 0.75);
        let (share, bar) = match state {
            State::Measuring => {
                (self.elapsed().as_secs_f64() / planned.map_or(1.0, |planned| planned.as_secs_f64()), hue)
            }
            State::Ended(StageStatus::Complete | StageStatus::Partial) => (1.0, hue),
            State::Ended(StageStatus::Failed) => (1.0, palette.err.fg.unwrap_or(hue)),
            State::Ended(StageStatus::Stopped) => (self.reached(stage), hue),
            _ => (0.0, hue),
        };
        let inner = area.width.saturating_sub(2);
        let halves = (share.clamp(0.0, 1.0) * f64::from(inner) * 2.0).round() as u16;
        let mut rule = Line::default();
        for cell in 0..inner {
            match () {
                _ if state == State::Warmup && self.sweeping(cell, inner) => rule.push('━', Style::fg(hue)),
                _ if state == State::Ended(StageStatus::Partial) && cell % 3 == 2 => rule.push(' ', Style::default()),
                _ if 2 * cell + 2 <= halves => rule.push('━', Style::fg(bar)),
                _ if 2 * cell + 1 == halves => rule.push('╸', Style::fg(bar)),
                _ => rule.push('━', Style::fg(track)),
            }
        }
        canvas.line(area.x + 1, area.y, inner, &rule);
        let (text, style) = self.chip_status(stage, state, planned, inner);
        let mark = Line::styled(icon(stage), if on { palette.stage(stage) } else { palette.muted });
        let name = mark
            .and(" ", Style::default())
            .and(words::label(stage), if on { palette.text } else { palette.muted });
        let room = inner.saturating_sub(text::width(&text) as u16 + 1);
        canvas.line(area.x + 1, area.y + 1, room, &name.fit(usize::from(room)));
        canvas.right(area.x + 1, area.y + 1, inner, &Line::styled(text, style));
        if !self.running() {
            canvas.hit(area, Target::Chip(stage));
        }
    }

    /// The chip's end: the planned time, the time into it, a check, or its state.
    fn chip_status(&self, stage: Stage, state: State, planned: Option<Duration>, width: u16) -> (String, Style) {
        let palette = &self.palette;
        match state {
            State::Measuring => {
                let elapsed = format!("{:.1}", self.elapsed().as_secs_f64());
                let long = planned.map(|planned| format!("{elapsed} / {}", words::setting(planned)));
                let fits =
                    |long: &String| text::width(long) + text::width(words::label(stage)) + 5 <= usize::from(width);
                (long.filter(fits).unwrap_or(format!("{elapsed} s")), palette.text)
            }
            State::Warmup => ("Warmup".into(), palette.muted),
            State::Ended(StageStatus::Complete) => ("✓".into(), palette.ok),
            State::Ended(StageStatus::Partial) => ("Partial".into(), palette.warn),
            State::Ended(StageStatus::Failed) => ("Failed".into(), Style { bold: false, ..palette.err }),
            State::Ended(StageStatus::Stopped) => ("Stopped".into(), palette.muted),
            State::Pending => (planned.map_or("Skipped".into(), words::setting), palette.muted),
        }
    }

    /// Whether a warming chip's sweep covers `cell` of `cells` now: 45% of the rule crossing it every 1.1 s.
    fn sweeping(&self, cell: u16, cells: u16) -> bool {
        let phase = self.now.saturating_duration_since(self.since).as_secs_f64() / 1.1 % 1.0;
        let head = (-0.45 + phase * 1.9) * f64::from(cells);
        let at = f64::from(cell);
        at >= head && at < head + 0.45 * f64::from(cells)
    }

    /// The latency lanes in `area`: a rule and head in the latency hue, the column heads, a lane each `pitch` rows, and
    /// the axis. A lane is its figures, a box plot (10th to 90th percentile, whiskers to the fastest and slowest, the
    /// median a tick and the latest reply a mark under it while live) and the latency its load added.
    fn lanes_area(&self, canvas: &mut Canvas, area: Rect, lanes: &[Lane], pitch: u16) {
        const LABEL: u16 = 15;
        const FIGURE: u16 = 10;
        let palette = &self.palette;
        let hue = palette.stage(Stage::Latency);
        canvas.line(area.x, area.y, area.width, &Line::styled("━".repeat(usize::from(area.width)), hue));
        let head = Line::styled(icon(Stage::Latency), hue).and(" Latency", palette.heading);
        canvas.line(area.x, area.y + 1, area.width, &head);
        if self.several() {
            let to = format!("to {}  l switches", self.name(self.latency_server()));
            canvas.right(area.x, area.y + 1, area.width, &Line::styled(to, palette.muted));
        }
        let jitter = area.width >= LABEL + 4 * FIGURE + 34;
        let plot_x = area.x + LABEL + if jitter { 3 } else { 2 } * FIGURE + 3;
        let plot_width = area.right().saturating_sub(plot_x + FIGURE + 7);
        let heads = ["Median", "Jitter", "Timeouts"]
            .into_iter()
            .filter(|head| jitter || *head != "Jitter");
        for (index, head) in heads.enumerate() {
            canvas.right(
                area.x + LABEL + index as u16 * FIGURE,
                area.y + 2,
                FIGURE,
                &Line::styled(head, palette.muted),
            );
        }
        canvas.right(area.x, area.y + 2, area.width, &Line::styled("Added", palette.muted));
        let scale = lane_scale(lanes);
        let at =
            |ms: f64| plot_x + ((ms / scale).clamp(0.0, 1.0) * f64::from(plot_width.saturating_sub(1))).round() as u16;
        let ms = |duration: Duration| duration.as_secs_f64() * 1e3;
        let idle = lanes
            .iter()
            .find(|lane| lane.stage == Stage::Latency)
            .and_then(|lane| lane.summary?.p50);
        for (index, lane) in lanes.iter().enumerate() {
            let y = area.y + 3 + index as u16 * pitch;
            let ground = if lane.live {
                palette.wash(lane.stage, 0.10, palette.canvas())
            } else {
                palette.canvas()
            };
            if lane.live {
                canvas.fill(Rect::new(area.x, y, area.width, 1), ground);
            }
            let name = Line::styled(icon(lane.stage), palette.stage(lane.stage))
                .and(" ", Style::default())
                .and(words::compact_population(lane.stage), palette.text);
            canvas.line(area.x, y, LABEL, &name);
            let summary = lane.summary.filter(|summary| summary.replies + summary.timeouts > 0);
            let missing = || Line::styled(MISSING, palette.muted);
            let mut cells = vec![
                summary
                    .and_then(|s| s.p50)
                    .map_or_else(missing, |m| Line::styled(words::ms(m), palette.value)),
            ];
            if jitter {
                let value = summary.and_then(|s| s.jitter.filter(|_| s.jitter_pairs > 0));
                cells.push(value.map_or_else(missing, |jitter| Line::styled(words::ms(jitter), palette.text)));
            }
            cells.push(summary.and_then(|s| s.timeout_ratio()).map_or_else(missing, |ratio| {
                Line::styled(percent(ratio), if ratio >= 0.05 { palette.warn } else { palette.text })
            }));
            for (column, cell) in cells.iter().enumerate() {
                canvas.right(area.x + LABEL + column as u16 * FIGURE, y, FIGURE, cell);
            }
            let added = match (lane.stage, summary.and_then(|s| s.p50), idle) {
                (Stage::Latency, Some(_), _) => Line::styled("Baseline", palette.stage(Stage::Latency)),
                (stage, Some(median), Some(idle)) => {
                    Line::styled(format!("{} ms", format::added(ms(median) - ms(idle))), palette.stage(stage))
                }
                _ => missing(),
            };
            canvas.right(area.x, y, area.width, &added);
            let (Some(summary), true) = (summary, plot_width >= 12) else { continue };
            let Some(spread) = summary.spread else { continue };
            let trace = palette.trace(lane.stage);
            let whisker = Style::fg(mix(trace, ground, 0.7));
            if let (Some(idle), true) = (idle, lane.stage != Stage::Latency) {
                let line = Style::fg(mix(palette.trace(Stage::Latency), ground, 0.55));
                canvas.line(at(ms(idle)), y, 1, &Line::styled("┊", line));
            }
            let (from, to) = (at(ms(spread.min)), at(ms(spread.max).min(scale)));
            canvas.line(from, y, to - from + 1, &Line::styled("─".repeat(usize::from(to - from) + 1), whisker));
            canvas.line(from, y, 1, &Line::styled("╶", whisker));
            if ms(spread.max) > scale {
                let end = plot_x + plot_width - 1;
                canvas.line(end, y, 1, &Line::styled("▸", Style::fg(trace)));
                canvas.line(end + 2, y, 6, &Line::styled(format::ms(ms(spread.max)), palette.muted));
            } else {
                canvas.line(to, y, 1, &Line::styled("╴", whisker));
            }
            let (box_from, box_to) = (at(ms(spread.p10)), at(ms(spread.p90).min(scale)));
            let span = box_to - box_from + 1;
            let boxed = mix(trace, ground, 0.38);
            match canvas.profile() >= Profile::Ansi256 {
                true => canvas.fill(Rect::new(box_from, y, span, 1), boxed),
                false => {
                    _ = canvas.line(box_from, y, span, &Line::styled("━".repeat(usize::from(span)), Style::fg(trace)))
                }
            }
            if let Some(median) = summary.p50 {
                let x = at(ms(median));
                let tick = if (box_from..=box_to).contains(&x) {
                    Style { bg: Some(boxed), ..palette.value }
                } else {
                    palette.value
                };
                canvas.line(x, y, 1, &Line::styled("┃", tick));
            }
            if let (Some(latest), true, 2) = (lane.latest, lane.live, pitch) {
                canvas.line(at(ms(latest)), y + 1, 1, &Line::styled("▴", Style::fg(trace)));
            }
        }
        if plot_width >= 12 {
            let axis = area.y + 3 + lanes.len() as u16 * pitch;
            canvas.line(plot_x, axis, 1, &Line::styled("0", palette.muted));
            canvas.center(plot_x, axis, plot_width, &Line::styled(gauge_tick(scale / 2.0), palette.muted));
            let end = Line::styled(format!("{} ms", gauge_tick(scale)), palette.muted);
            canvas.right(plot_x, axis, plot_width + 3, &end);
        }
    }

    /// A card per stage, as many to a row as keep each at least 30 columns wide; a short area keeps them in one row.
    fn cards(&self, canvas: &mut Canvas, area: Rect) {
        let plan = self.plan();
        if plan.is_empty() || area.height < 2 {
            return;
        }
        let narrowest = if area.height < 16 { 18 } else { 30 };
        let mut per = plan.len() as u16;
        while per > 1 && area.width.saturating_sub(2 * (per - 1)) / per < narrowest {
            per = per.div_ceil(2);
        }
        let rows = (plan.len() as u16).div_ceil(per);
        let height = area.height.saturating_sub(rows - 1) / rows;
        let width = area.width.saturating_sub(2 * (per - 1)) / per;
        let scales = self.scales();
        for (index, &(stage, duration)) in plan.iter().enumerate() {
            let (column, row) = (index as u16 % per, index as u16 / per);
            let x = area.x + column * (width + 2);
            let card = Rect::new(
                x,
                area.y + row * (height + 1),
                if column + 1 == per { area.right() - x } else { width },
                height,
            );
            self.card(canvas, card, stage, duration, scales);
        }
    }

    /// The cards' tops: the rate charts' in bytes per second, shared, and the reply tracks' in milliseconds, the idle
    /// one's its own and the loaded ones' shared, each the ladder step over 2.5× the 75th percentile of its replies'
    /// slowest, so their bodies keep their shape.
    fn scales(&self) -> Scales {
        let run = self.view.run.as_ref();
        let peak = run.map_or(0.0, |run| run.peak);
        let rate = ceil_step(peak.max(1.0) * 8.0 * 1.03, &CHART_STEPS) / 8.0;
        let series = run.and_then(|run| run.rtt.iter().find(|(id, _)| Some(id) == self.latency_server()));
        let top = |idle: bool| {
            let windows = self
                .plan()
                .into_iter()
                .filter(|(stage, _)| (*stage == Stage::Latency) == idle);
            let spans = windows.filter_map(|(stage, duration)| {
                let open = self.window(run?, stage)?.0;
                Some((open, open + duration.as_secs_f64()))
            });
            let mut highs: Vec<f64> = spans
                .flat_map(|span| series.map_or(&[][..], |(_, series)| between(series, span)))
                .filter(|point| point.value.is_some())
                .map(|point| point.peak)
                .collect();
            highs.sort_by(f64::total_cmp);
            let p75 = highs.get(highs.len() * 3 / 4).copied().unwrap_or(0.0);
            ceil_step((p75 * 2.5).max(1.0), &LADDER)
        };
        Scales { rate, idle: top(true), loaded: top(false) }
    }

    /// One stage's card: a head washed in its hue while it runs and once done, its figure, a quiet line, its traces on
    /// a field of its hue, and its facts. Its layout never changes, so nothing moves between Start and the result.
    fn card(&self, canvas: &mut Canvas, area: Rect, stage: Stage, duration: Duration, scales: Scales) {
        let palette = &self.palette;
        let state = self.stage_state(stage);
        let tile = palette.surface;
        canvas.fill(area, tile);
        let head = match state {
            State::Measuring | State::Warmup => palette.wash(stage, 0.24, tile),
            State::Ended(_) => palette.wash(stage, 0.12, tile),
            State::Pending => palette.raised,
        };
        canvas.fill(Rect::new(area.x, area.y, area.width, 1), head);
        let inner = area.width.saturating_sub(2);
        let mark = Line::styled(icon(stage), palette.stage(stage)).and(" ", Style::default());
        canvas.line(area.x + 1, area.y, inner, &mark.and(words::label(stage), palette.heading));
        let (status, style) = self.card_status(state);
        canvas.right(area.x + 1, area.y, inner, &Line::styled(status, style));
        let run = self.view.run.as_ref();
        let result = run.and_then(|run| run.results.iter().rfind(|result| result.stage == stage));
        let unit = Unit::of(run.map_or(0.0, |run| run.peak));
        // Head, figure, quiet line, the traces' field and the facts; a roomy card adds a row of air around them, and a
        // short one gives up its traces, then the facts that do not fit.
        let roomy = u16::from(area.height >= 16);
        let mut y = area.y + 1 + roomy;
        for line in self.card_lines(stage, state, unit, inner) {
            if y < area.bottom() {
                canvas.line(area.x + 1, y, inner, &line);
            }
            y += 1;
        }
        let mut facts = self.facts(stage, result, unit);
        facts.truncate(usize::from(area.bottom().saturating_sub(y + roomy)));
        let first = area.bottom().saturating_sub(facts.len() as u16 + roomy);
        let (top, bottom) = (y + roomy, first.saturating_sub(u16::from(!facts.is_empty())));
        if bottom >= top + 2 {
            let field = Rect::new(area.x + 1, top, inner, bottom - top);
            canvas.fill(field, palette.wash(stage, 0.05, tile));
            self.traces(canvas, field, stage, duration, scales);
        }
        for (index, (label, value)) in facts.into_iter().enumerate() {
            let y = first + index as u16;
            let style = if value == MISSING { palette.muted } else { palette.text };
            let value = Line::styled(value, style);
            let room = inner.saturating_sub(value.width() as u16 + 1);
            canvas.line(area.x + 1, y, room, &Line::styled(label, palette.muted).fit(usize::from(room)));
            canvas.right(area.x + 1, y, inner, &value);
        }
    }

    /// The card's head end: what the stage is doing, or how it ended.
    fn card_status(&self, state: State) -> (&'static str, Style) {
        let palette = &self.palette;
        match state {
            State::Measuring => ("Running", palette.text),
            State::Warmup => ("Warming up", palette.muted),
            State::Ended(StageStatus::Complete) => ("✓", palette.ok),
            State::Ended(StageStatus::Partial) => ("Partial", palette.warn),
            State::Ended(StageStatus::Failed) => ("Failed", Style { bold: false, ..palette.err }),
            State::Ended(StageStatus::Stopped) => ("Stopped", palette.muted),
            State::Pending => ("", palette.muted),
        }
    }

    /// The card's figure, and its quiet line: the wire rate, the jitter, a failure's reason or the warmup.
    fn card_lines(&self, stage: Stage, state: State, unit: Unit, width: u16) -> [Line; 2] {
        let palette = &self.palette;
        let run = self.view.run.as_ref();
        let result = run.and_then(|run| run.results.iter().rfind(|result| result.stage == stage));
        let missing = Line::styled(MISSING, palette.muted);
        let figure =
            |value: String, unit: &str| Line::styled(value, palette.value).and(format!(" {unit}"), palette.muted);
        let mut quiet = Line::default();
        let value = if stage.directions().is_empty() {
            let summary = self.population(stage);
            if let Some(jitter) = summary.and_then(|s| s.jitter.filter(|_| s.jitter_pairs > 0)) {
                quiet = Line::styled("jitter ", palette.muted).and(words::ms(jitter), palette.text);
            }
            let median = summary.and_then(|summary| summary.p50);
            let shown = median.or_else(|| self.latest().filter(|_| state == State::Measuring));
            shown.map_or(missing.clone(), |rtt| figure(format::latency(rtt.as_secs_f64() * 1e3), "ms"))
        } else {
            let rate = |direction| match result.and_then(|result: &StageResult| result.throughput[direction]?.rate) {
                Some(rate) => Some(rate.mean),
                None if state == State::Measuring => {
                    let shown = self.live.shown[direction];
                    (shown.to > 0.0).then(|| shown.value(self.now))
                }
                None => None,
            };
            let hue = palette.stage(stage);
            match stage.directions() {
                [direction] => rate(*direction).map_or(missing.clone(), |rate| figure(unit.figure(rate), unit.name)),
                [down, up] => match (rate(*down), rate(*up)) {
                    (None, None) => missing.clone(),
                    (down, up) => {
                        let side = |arrow: &str, rate: Option<f64>| {
                            let shown =
                                rate.map_or(missing.clone(), |rate| Line::styled(unit.figure(rate), palette.value));
                            Line::styled(arrow, hue).with(shown)
                        };
                        let pair = side("↓ ", down).and("   ", Style::default()).with(side("↑ ", up));
                        pair.and(format!(" {}", unit.name), palette.muted)
                    }
                },
                _ => missing.clone(),
            }
        };
        if let Some(result) = result.filter(|_| state == State::Ended(StageStatus::Complete)) {
            let mean = result.mean();
            if let Some(wire) = wire::wire_rate(result, &self.view.servers).filter(|wire| *wire >= mean * 1.005) {
                quiet = Line::styled("wire ", palette.muted)
                    .and(format!("{} {}", unit.figure(wire), unit.name), palette.text);
                let overhead = format!("  +{:.1}%", (wire / mean - 1.0) * 100.0);
                if quiet.width() + overhead.len() <= usize::from(width) {
                    quiet = quiet.and(overhead, palette.muted);
                }
            }
        }
        quiet = match state {
            State::Ended(status @ (StageStatus::Failed | StageStatus::Partial)) => {
                let (fallback, style) = match status {
                    StageStatus::Failed => ("Failed", Style { bold: false, ..palette.err }),
                    _ => ("Partial", palette.warn),
                };
                Line::styled(self.failure(stage).unwrap_or(fallback), style)
            }
            State::Warmup => Line::styled(self.spinner(), palette.accent).and(" not counted yet", palette.muted),
            _ => quiet,
        };
        [value, quiet]
    }

    /// Why the stage failed: the latency server's first failure, else any server's.
    fn failure(&self, stage: Stage) -> Option<&'static str> {
        let run = self.view.run.as_ref()?;
        let result = run.results.iter().rfind(|result| result.stage == stage)?;
        let counted = |failure: &&ServerFailure| failure.failure.reason != FailureReason::InsufficientEvidence;
        let mut failures = result.failures.iter().filter(counted);
        let own = failures
            .clone()
            .find(|failure| Some(&failure.server) == self.latency_server());
        Some(own.or_else(|| failures.next())?.failure.reason.label())
    }

    /// The card's facts as label and value, "—" until known.
    fn facts(&self, stage: Stage, result: Option<&StageResult>, unit: Unit) -> Vec<(&'static str, String)> {
        let missing = || MISSING.to_owned();
        if stage.directions().is_empty() {
            let summary = self.population(stage);
            let range = summary
                .and_then(|summary| summary.spread)
                .map_or_else(missing, |spread| {
                    let (low, high) = (spread.min.as_secs_f64() * 1e3, spread.max.as_secs_f64() * 1e3);
                    format!("{}–{} ms", format::latency(low), format::ms(high))
                });
            let p95 = summary.and_then(|summary| summary.p95).map_or_else(missing, words::ms);
            let replies = summary
                .filter(|s| s.replies > 0)
                .map_or_else(missing, |s| words::count(s.replies));
            let timeouts = summary.and_then(|s| s.timeout_ratio()).map_or_else(missing, percent);
            return vec![("Range", range), ("95th percentile", p95), ("Replies", replies), ("Timeouts", timeouts)];
        }
        let measured = result.filter(|result| !result.measured.is_zero());
        let bytes = |direction| {
            let throughput = measured.and_then(|result: &StageResult| result.throughput[direction]);
            throughput.map_or_else(missing, |throughput| format::bytes(throughput.bytes))
        };
        let duration = measured.map_or_else(missing, |result| words::clock(result.measured));
        match stage.directions() {
            [direction] => {
                let rate = measured
                    .and_then(|result| result.throughput[*direction]?.rate)
                    .filter(|rate| rate.peak > 0.0);
                let peak = rate.map_or_else(missing, |rate| format!("{} {}", unit.figure(rate.peak), unit.name));
                vec![("Peak", peak), ("Transferred", bytes(*direction)), ("Duration", duration)]
            }
            [down, up] => vec![("↓ Transferred", bytes(*down)), ("↑ Transferred", bytes(*up)), ("Duration", duration)],
            _ => Vec::new(),
        }
    }

    /// The stage's traces on its field: a transfer's rate line over its loaded replies, or the idle replies alone.
    fn traces(&self, canvas: &mut Canvas, field: Rect, stage: Stage, duration: Duration, scales: Scales) {
        let Some(run) = self.view.run.as_ref() else { return };
        let Some(window) = self.window(run, stage) else { return };
        let span = (window.0, window.0 + duration.as_secs_f64());
        let palette = &self.palette;
        let hue = palette.trace(stage);
        let rtt = run
            .rtt
            .iter()
            .find(|(id, _)| Some(id) == self.latency_server())
            .map(|(_, series)| series);
        if stage.directions().is_empty() {
            if let Some(series) = rtt {
                chart::bars(canvas, field, between(series, span), (scales.idle, span), hue);
            }
            return;
        }
        let loaded = self.config.loaded_latency && rtt.is_some() && field.height >= 5;
        let rates = Rect::new(field.x, field.y, field.width, field.height - if loaded { 2 } else { 0 });
        let live = self.stage_state(stage) == State::Measuring;
        let tip = palette.text.fg.unwrap_or(hue);
        for (index, &direction) in stage.directions().iter().enumerate() {
            let head = (window.0 + self.elapsed().as_secs_f64(), self.live.shown[direction].value(self.now));
            let head = Some(head).filter(|&(_, value)| live && value > 0.0);
            let color = if index == 0 { hue } else { mix(hue, palette.surface, 0.55) };
            let points = between(&run.throughput[direction], span);
            // The area fills under the first direction only, so a second line stays readable over it.
            let field = (index == 0).then(|| palette.wash(stage, 0.05, palette.surface));
            chart::line(canvas, rates, points, head, (scales.rate, span), (color, tip, field));
        }
        if let (true, Some(series)) = (loaded, rtt) {
            let track = Rect::new(field.x, field.bottom() - 2, field.width, 2);
            chart::bars(
                canvas,
                track,
                between(series, span),
                (scales.loaded, span),
                mix(hue, palette.surface, 0.8),
            );
        }
    }

    /// What the dial shows: the live stage while one measures, the result once the run ends, at rest otherwise.
    pub(super) fn dial(&self) -> Dial {
        let palette = &self.palette;
        let run = self.view.run.as_ref();
        let peak = run.map_or(0.0, |run| run.peak);
        let unit = Unit::of(peak);
        let gauge = gauge_ceiling(if peak > 0.0 { peak } else { 12.5e6 });
        let rate_ticks: [String; 5] =
            std::array::from_fn(|i| gauge_tick(gauge_value(i as f64 / 4.0, gauge) * 8.0 / unit.divisor));
        let ms_scale = self.live.rtt_scale.max(1.0);
        let ms_ticks: [String; 5] = std::array::from_fn(|i| gauge_tick(i as f64 / 4.0 * ms_scale));
        let label =
            |stage: Stage| Line::styled(format!("{} {}", icon(stage), words::label(stage)), palette.stage(stage));
        let border = palette.border.fg.unwrap_or(palette.canvas());
        let muted = |text: &str| Line::styled(text, palette.muted);
        let at_rest = |ticks: [String; 5], note: Line| Dial {
            arcs: Vec::new(),
            track: border,
            ticks,
            label: Line::default(),
            value: MISSING.into(),
            unit: String::new(),
            note,
        };
        let Some(run) = run else {
            let note = match self.config.url {
                None => "Add a server to measure your connection",
                Some(_) => "Ready to measure your connection",
            };
            return at_rest(rate_ticks, muted(note));
        };
        if self.running() {
            let Some((plan, window)) = &run.stage else {
                return at_rest(rate_ticks, muted("Verifying the selected paths"));
            };
            let stage = plan.stage;
            let track = palette.wash(stage, 0.22, border);
            let ticks = if stage == Stage::Latency { ms_ticks.clone() } else { rate_ticks.clone() };
            let needle = |to| {
                vec![Arc {
                    to: self.live.needle(to, self.now),
                    color: palette.trace(stage),
                }]
            };
            if window.is_none() {
                let warming = at_rest(ticks, muted("Warming up; not counted"));
                return Dial { track, label: label(stage), arcs: needle(0.0), ..warming };
            }
            if stage == Stage::Latency {
                let latest = self.latest();
                return Dial {
                    arcs: needle(latest.map_or(0.0, |rtt| rtt.as_secs_f64() * 1e3 / ms_scale)),
                    track,
                    ticks,
                    label: label(stage),
                    value: latest.map_or(MISSING.into(), |rtt| format::latency(rtt.as_secs_f64() * 1e3)),
                    unit: "ms".into(),
                    note: self.timeout_streak(),
                };
            }
            let value: f64 = stage
                .directions()
                .iter()
                .map(|&direction| self.live.shown[direction].value(self.now))
                .sum();
            return Dial {
                arcs: needle(gauge_fraction(value, gauge)),
                track,
                ticks,
                label: label(stage),
                value: if value > 0.0 { unit.figure(value) } else { MISSING.into() },
                unit: unit.name.into(),
                note: Line::default(),
            };
        }
        // Finished: every transfer stage's arc, swept in from where the needle stood, and the first one's figure.
        let mut results = run.plan.iter().filter_map(|&(stage, _)| {
            let mean = run.results.iter().rfind(|result| result.stage == stage)?.mean();
            (!stage.directions().is_empty() && mean > 0.0).then_some((stage, mean))
        });
        if let Some((stage, mean)) = results.next() {
            let arcs = std::iter::once((stage, mean)).chain(results).map(|(stage, mean)| Arc {
                to: self.live.swept(gauge_fraction(mean, gauge), self.now),
                color: palette.trace(stage),
            });
            return Dial {
                arcs: arcs.collect(),
                track: border,
                ticks: rate_ticks,
                label: label(stage),
                value: unit.figure(mean),
                unit: unit.name.into(),
                note: self.outcome_note(),
            };
        }
        match self.population(Stage::Latency).and_then(|summary| summary.p50) {
            Some(median) => Dial {
                arcs: vec![Arc {
                    to: median.as_secs_f64() * 1e3 / ms_scale,
                    color: palette.trace(Stage::Latency),
                }],
                track: border,
                ticks: ms_ticks,
                label: label(Stage::Latency),
                value: format::latency(median.as_secs_f64() * 1e3),
                unit: "ms".into(),
                note: self.outcome_note(),
            },
            None => at_rest(rate_ticks, self.outcome_note()),
        }
    }

    /// Under a finished run's dial: why it stopped or failed, if it did.
    fn outcome_note(&self) -> Line {
        let palette = &self.palette;
        let run = self.view.run.as_ref();
        match run.and_then(|run| run.outcome) {
            Some(Outcome::Stopped) => Line::styled("Stopped early", palette.muted),
            Some(Outcome::Failed) => {
                let error = run.and_then(|run| run.error.as_ref());
                Line::styled(error.map_or("Nothing was measured", |error| error.text.as_str()), palette.warn)
            }
            _ => Line::default(),
        }
    }

    /// A lane for idle latency and, with loaded latency on, each transfer stage, in plan order.
    pub(super) fn lanes(&self) -> Vec<Lane> {
        let loaded = |stage: Stage| stage == Stage::Latency || self.config.loaded_latency;
        let lanes = self
            .plan()
            .into_iter()
            .filter(|(stage, _)| loaded(*stage))
            .map(|(stage, _)| {
                let live = self.stage_state(stage) == State::Measuring;
                Lane {
                    stage,
                    summary: self.population(stage),
                    latest: self.latest().filter(|_| live),
                    live,
                }
            });
        lanes.collect()
    }

    /// The stages shown: the run's plan once it started, else the settings'.
    pub(super) fn plan(&self) -> Vec<(Stage, Duration)> {
        match &self.view.run {
            Some(run) if !run.plan.is_empty() => run.plan.clone(),
            _ => self.config.plan(),
        }
    }
}

/// The lanes' shared axis: the ladder step over their 90th percentiles with a quarter's room, at least 1 ms.
fn lane_scale(lanes: &[Lane]) -> f64 {
    let p90 = |lane: &Lane| Some(lane.summary?.spread?.p90.as_secs_f64() * 1e3);
    let top = lanes.iter().filter_map(p90).fold(0.0, f64::max);
    ceil_step((top * 1.25).max(1.0), &LADDER)
}

/// A share of probes as the browser shows it: two decimals under 1%, one from there.
fn percent(ratio: f64) -> String {
    let places = if ratio > 0.0 && ratio < 0.01 { 2 } else { 1 };
    format!("{:.places$}%", ratio * 100.0)
}

/// The series' points over `window` seconds.
fn between(series: &Series, (t0, t1): (f64, f64)) -> &[Point] {
    let at = |t: f64| series.points.partition_point(|point| point.at.as_secs_f64() < t);
    &series.points[at(t0)..at(t1 + 1e-9)]
}
