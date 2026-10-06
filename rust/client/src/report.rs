//! A finished run as the printed report, per-server shares and issues with several servers, and headless progress.
mod details;
pub mod vocabulary;

pub use details::details;

use crate::{
    events::{Event, Run, View},
    measure::{format, latency::Population},
    model::{Direction, Outcome, ServerFailure, Stage, StageResult, StageStatus, Throughput},
    text::{Line, Style, wrap},
    tui::theme::Palette,
};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::time::Duration;
use vocabulary::*;

/// The width of a report that does not reach a terminal.
pub const WIDTH: usize = 100;
const PARTIAL: &str = "Partial";
const ADDED_NOTE: &str = "Added: loaded median minus idle median, same server.";
const SIGNED_OUT: &str = "Sign-in expired. Checking the selected servers…";

/// The report of the view's finished run; none for a run that never started or lost its sign-in.
pub fn report(view: &View, width: usize, palette: &Palette) -> Vec<Line> {
    let reported = view
        .run
        .as_ref()
        .filter(|run| run.outcome.is_some() && unreported(view).is_none());
    let Some(run) = reported else { return Vec::new() };
    let report = Report { view, run, focus: run.focus.as_ref(), width, palette };
    let focus = run.focus.as_ref().filter(|_| report.several());
    let heading = focus.map_or("Latency".to_owned(), |focus| format!("Latency to {}", report.name(focus)));
    let mut blocks = vec![vec![report.header()]];
    if let Some((latency, failures, added)) = report.results(&heading) {
        blocks.extend([report.throughput(), latency, failures, report.notes(added)]);
    }
    if report.several() {
        blocks.push(report.details(false));
    }
    let error = run.error.as_ref();
    blocks.extend(error.map(|error| vec![Line::styled(&error.text, palette.err)]));
    blocks.retain(|block| !block.is_empty());
    blocks.join(&Line::default()).into_iter().map(Line::trimmed).collect()
}

/// The finished run's results as the interface shows them, `focus` the latency server; none when nothing was measured.
pub fn results(view: &View, focus: Option<&ServerId>, width: usize, palette: &Palette) -> Vec<Line> {
    let measured = view.run.as_ref().filter(|run| run.at.is_some());
    let Some(run) = measured else { return Vec::new() };
    let report = Report { view, run, focus, width, palette };
    let Some((latency, failures, _)) = report.results("Latency") else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for &(stage, _) in run.plan.iter().filter(|(stage, _)| !stage.directions().is_empty()) {
        let rates = report.result(stage).map(mean_rates).filter(|rates| !rates.is_empty());
        let mut cell = Line::plain(rates.as_deref().unwrap_or(report.unmeasured(stage)));
        if report.status(stage) == Some(StageStatus::Partial) {
            cell = cell.and("  ", Style::default()).and(PARTIAL, palette.warn);
        }
        rows.push(vec![Line::styled(compact_stage(stage), palette.stage(stage)), cell]);
    }
    let scope = if report.several() { "All servers" } else { "" };
    let throughput = match rows.is_empty() {
        true => Vec::new(),
        false => report.grid(&["Throughput", scope], rows),
    };
    [throughput, latency, failures].concat()
}

/// Why the view's run has no report: it never started, or its sign-in expired.
pub fn unreported(view: &View) -> Option<String> {
    let run = view.run.as_ref()?;
    let error = run.error.as_ref();
    let signed_out = error.filter(|error| error.reason == FailureReason::SignInRequired);
    Some(match (error, signed_out) {
        _ if run.at.is_some() => return signed_out.map(|_| SIGNED_OUT.into()),
        (_, Some(error)) => error.text.clone(),
        (Some(error), None) => format!("Test could not start: {}", error.text),
        (None, None) => "Test stopped before it started.".into(),
    })
}

/// The stderr line a run without the interface writes for `event`.
pub fn progress(event: &Event) -> Option<String> {
    let Event::Measuring(stage) = event else { return None };
    Some(format!("{}…", label(*stage)))
}

struct Report<'a> {
    view: &'a View,
    run: &'a Run,
    /// The latency server.
    focus: Option<&'a ServerId>,
    width: usize,
    palette: &'a Palette,
}

impl Report<'_> {
    fn several(&self) -> bool {
        self.view.servers.len() > 1
    }

    fn name(&self, id: &ServerId) -> String {
        let server = self.view.servers.iter().find(|server| server.id == *id);
        server.map_or_else(|| id.as_str().to_owned(), |server| server.name.clone())
    }

    fn result(&self, stage: Stage) -> Option<&StageResult> {
        self.run.results.iter().rfind(|result| result.stage == stage)
    }

    fn status(&self, stage: Stage) -> Option<StageStatus> {
        Some(self.result(stage)?.status(self.focus))
    }

    /// What a planned stage shows without a value.
    fn unmeasured(&self, stage: Stage) -> &'static str {
        self.status(stage)
            .map_or("Skipped", |status| [MISSING, PARTIAL, "Failed", "Stopped"][status as usize])
    }

    /// The latency server's population in `stage`.
    fn population(&self, stage: Stage) -> Option<Population> {
        let result = self.result(stage)?;
        let server = result.servers.iter().find(|server| Some(&server.server) == self.focus);
        server?.latency
    }

    fn header(&self) -> Line {
        let mut facts = match self.view.servers.len() {
            0 => Vec::new(),
            1 => vec![self.view.servers[0].name.clone()],
            servers => vec![format!("{servers} servers")],
        };
        facts.push(clock(self.run.elapsed));
        let results = self.run.results.iter();
        let directions = results.flat_map(|result| [result.throughput.down, result.throughput.up]);
        let total: u64 = directions.flatten().map(|throughput| throughput.bytes).sum();
        facts.extend((total > 0).then(|| format::bytes(total)));
        let outcome = self.run.outcome.unwrap_or(Outcome::Failed);
        Line::styled("Graphite Meter", self.palette.heading)
            .and("  ", Style::default())
            .and(outcome_label(outcome), self.palette.outcome(outcome))
            .and("  ", Style::default())
            .and(facts.join(" · "), self.palette.muted)
    }

    /// Each planned direction's mean rate and facts.
    fn throughput(&self) -> Vec<Line> {
        let palette = self.palette;
        let mut rows = Vec::new();
        for &(stage, _) in &self.run.plan {
            let hue = palette.stage(stage);
            for &direction in stage.directions() {
                let named = Line::styled(arrow(direction), hue)
                    .and(" ", Style::default())
                    .and(label(stage), palette.text);
                let result = self.result(stage).map(|result| (result, result.throughput[direction]));
                let (value, facts) = match result {
                    Some((result, Some(throughput @ Throughput { rate: Some(rate), .. }))) => {
                        let facts = throughput_facts(throughput, result.measured, direction, true);
                        (Line::styled(format::rate(rate.mean), hue.bold()), facts)
                    }
                    Some((_, Some(_))) => (Line::styled(MISSING, palette.muted), Vec::new()),
                    _ => (Line::styled(self.unmeasured(stage), palette.muted), Vec::new()),
                };
                let partial = (self.status(stage) == Some(StageStatus::Partial)).then(|| PARTIAL.to_owned());
                rows.push((named, value, [partial.into_iter().collect(), facts].concat()));
            }
        }
        let label_width = rows.iter().map(|row| row.0.width()).max().unwrap_or(0);
        let value_width = rows.iter().map(|row| row.1.width()).max().unwrap_or(0);
        let indent = label_width + value_width + 2;
        let mut lines = Vec::new();
        for (label, value, facts) in rows {
            let first = label
                .pad(label_width)
                .and("  ", Style::default())
                .with(value.pad(value_width));
            let wrapped = wrap(&facts, self.width.saturating_sub(indent + 3).max(20));
            for (index, part) in wrapped.into_iter().enumerate() {
                let line = if index == 0 { first.clone() } else { Line::plain(" ".repeat(indent)) };
                lines.push(match part.strip_prefix(PARTIAL).filter(|_| index == 0) {
                    Some(rest) => line
                        .and("   ", Style::default())
                        .and(PARTIAL, palette.warn)
                        .and(rest, palette.muted),
                    None if part.is_empty() => line,
                    None => line.and("   ", Style::default()).and(part, palette.muted),
                });
            }
        }
        lines
    }

    /// The latency grid, the failures under it and whether a row shows Added; none when nothing was measured.
    fn results(&self, heading: &str) -> Option<(Vec<Line>, Vec<Line>, bool)> {
        let palette = self.palette;
        let idle = self.population(Stage::Latency).and_then(|idle| idle.median());
        let (mut rows, mut failures, mut added_shown, mut measured) = (Vec::new(), Vec::new(), false, false);
        for &(stage, _) in &self.run.plan {
            let result = self.result(stage);
            for &direction in stage.directions() {
                measured |= result.is_some_and(|result| result.throughput[direction].is_some());
                failures.extend(result.and_then(|result| self.throughput_failure(result, direction)));
            }
            let name = Line::styled(compact_population(stage), palette.stage(stage));
            let Some(population) = self.population(stage) else {
                if stage == Stage::Latency {
                    rows.push(vec![name, Line::plain(self.unmeasured(stage))]);
                }
                continue;
            };
            measured = true;
            let mut cells = latency_cells(&population, idle);
            if stage == Stage::Latency {
                cells[1].clear();
            }
            added_shown |= !cells[1].is_empty();
            rows.push([vec![name], cells.drain(..).map(Line::plain).collect()].concat());
            failures.extend(result.and_then(|result| self.latency_failure(result)));
        }
        if !measured {
            return None;
        }
        let mut headers = vec![heading, "Median", "Added", "P95", "Jitter", "Probe timeouts"];
        for row in &mut rows {
            row.resize(headers.len(), Line::default());
            if !added_shown {
                row.remove(2);
            }
        }
        if !added_shown {
            headers.remove(2);
        }
        let grid = if rows.is_empty() { Vec::new() } else { self.grid(&headers, rows) };
        Some((grid, failures, added_shown))
    }

    /// That a direction stopped in its window, or why every server left its stage.
    fn throughput_failure(&self, result: &StageResult, direction: Direction) -> Option<Line> {
        let label = direction_label(result.stage, direction);
        if result.stopped {
            result.throughput[direction]?;
            return Some(Line::styled(format!("{label} stopped."), self.palette.warn));
        }
        let mut failures = result.failures.iter().map(|failure| &failure.failure);
        let last = failures.rfind(|failure| failure.reason != FailureReason::InsufficientEvidence)?;
        let gone = result.servers.iter().all(|server| server.left);
        gone.then(|| Line::styled(format!("{label}: {}", last.reason.label()), self.palette.err))
    }

    /// That the latency server's population stopped, or why it ended early: its first failure in the stage.
    fn latency_failure(&self, result: &StageResult) -> Option<Line> {
        let label = population_label(result.stage);
        if result.stopped {
            return Some(Line::styled(format!("{label} stopped."), self.palette.warn));
        }
        let focus = self.focus?;
        let first = |failure: &&ServerFailure| {
            failure.server == *focus && failure.failure.reason != FailureReason::InsufficientEvidence
        };
        let failed = result.failures.iter().find(first);
        Some(Line::styled(format!("{label}: {}", failed?.failure.reason.label()), self.palette.err))
    }

    /// Latency populations with issues, the server's share of the round trips, and what Added means.
    fn notes(&self, added_shown: bool) -> Vec<Line> {
        let (mut notes, mut timing) = (Vec::new(), Vec::new());
        for &(stage, _) in &self.run.plan {
            let Some(population) = self.population(stage) else { continue };
            let (summary, result) = (population.summary, self.result(stage));
            let failed = result.and_then(|result| self.latency_failure(result)).is_some();
            if failed || summary.timeouts > 0 || summary.unresolved > 0 || summary.send_failures > 0 {
                let facts = latency_facts(summary, result.map_or(Duration::ZERO, |result| result.measured));
                notes.extend(self.note(&population_label(stage), &facts));
            }
            if let Some(paired) = summary.timing {
                let mut part = format!("{} {} of {}", compact_population(stage), ms(paired.handling), ms(paired.rtt));
                if paired.pairs != summary.replies {
                    part.push_str(&format!(" ({} pairs)", count(paired.pairs)));
                }
                timing.push(part);
            }
        }
        if !timing.is_empty() {
            notes.extend(self.note("Server handling of the mean round trip", &timing));
        }
        notes.extend(added_shown.then(|| Line::styled(ADDED_NOTE, self.palette.muted)));
        notes
    }

    /// The label and its facts, wrapped under the label when the first does not fit beside it.
    fn note(&self, label: &str, facts: &[String]) -> Vec<Line> {
        let room = self.width.saturating_sub(2);
        let first = format!("{label}: {}", facts[0]);
        let (mut lines, facts) = match crate::text::width(&first) <= room {
            true => (Vec::new(), [vec![first], facts[1..].to_vec()].concat()),
            false => (vec![format!("{label}:")], facts.to_vec()),
        };
        for line in wrap(&facts, room) {
            lines.push(if lines.is_empty() { line } else { format!("  {line}") });
        }
        let muted = |line| Line::styled(line, self.palette.muted);
        lines.into_iter().map(muted).collect()
    }

    /// Muted headers over text cells, or each row's facts under its name when the columns do not fit.
    fn grid(&self, headers: &[&str], rows: Vec<Vec<Line>>) -> Vec<Line> {
        let (muted, text) = (self.palette.muted, self.palette.text);
        let mut widths: Vec<_> = headers.iter().map(|header| crate::text::width(header)).collect();
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.width());
            }
        }
        if widths.iter().map(|width| width + 2).sum::<usize>().saturating_sub(2) <= self.width {
            let header = headers.iter().map(|header| Line::styled(*header, muted)).collect();
            let based = |row: Vec<Line>| row.into_iter().map(|cell| cell.based(text)).collect();
            let rows = std::iter::once(header).chain(rows.into_iter().map(based));
            let joined = |row: Vec<Line>| {
                let cells = row.into_iter().zip(&widths).map(|(cell, width)| cell.pad(*width));
                let joined = cells.reduce(|line, cell| line.and("  ", Style::default()).with(cell));
                joined.unwrap_or_default().trimmed()
            };
            return rows.map(joined).collect();
        }
        let mut lines = vec![Line::styled(headers[0], muted)];
        for row in rows {
            let facts = row[1..].iter().zip(&headers[1..]).filter(|(cell, _)| cell.width() > 0);
            let fact = |(cell, header): (&Line, &&str)| format!("{header} {}", cell.text()).trim().to_owned();
            let facts: Vec<_> = facts.map(fact).collect();
            lines.push(row[0].clone().based(text));
            let indented = |fact| Line::plain("  ").and(fact, muted);
            lines.extend(wrap(&facts, self.width.saturating_sub(2)).into_iter().map(indented));
        }
        lines
    }
}
