//! A finished run as the printed report: header, throughput, latency, failures and notes, and with several servers
//! each server's share and the run's issues; and the progress lines a run without the interface writes.
mod details;
pub mod vocabulary;

pub use details::details;

use crate::{
    events::{Event, Run, View},
    measure::{format, latency::Population},
    model::{Direction, Outcome, Scope, ServerResult, Stage, StageResult, StageStatus, Throughput},
    text::{Line, Style, wrap},
    tui::theme::Palette,
};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use vocabulary::*;

/// The width of a report that does not reach a terminal.
pub const WIDTH: usize = 100;
const ADDED_NOTE: &str = "Added: loaded median minus idle median, same server.";

/// The report of the view's finished run; none for a run that never started.
pub fn report(view: &View, width: usize, palette: &Palette) -> Vec<Line> {
    let Some(run) = view
        .run
        .as_ref()
        .filter(|run| run.at.is_some() && run.outcome.is_some())
    else {
        return Vec::new();
    };
    let report = Report { view, run, width, palette };
    let heading = match (report.several(), &run.focus) {
        (true, Some(focus)) => format!("Latency to {}", report.name(focus)),
        _ => "Latency".to_owned(),
    };
    let mut blocks = vec![vec![report.header()]];
    if let Some((latency, failures, added)) = report.results(&heading) {
        blocks.extend([report.throughput(), latency, failures, report.notes(added)]);
    }
    if report.several() {
        blocks.push(report.details(false));
    }
    blocks.extend(
        run.error
            .as_ref()
            .map(|error| vec![Line::styled(&error.text, palette.err)]),
    );
    blocks.retain(|block| !block.is_empty());
    blocks.join(&Line::default()).into_iter().map(Line::trimmed).collect()
}

/// Why the view's run never started, as a run without the interface ends.
pub fn unstarted(view: &View) -> Option<String> {
    let run = view.run.as_ref().filter(|run| run.at.is_none())?;
    Some(match &run.error {
        Some(error) if error.reason == FailureReason::SignInRequired => error.text.clone(),
        Some(error) => format!("Test could not start: {}", error.text),
        None => "Test stopped before it started.".into(),
    })
}

/// The stderr line a run without the interface writes for `event`.
pub fn progress(event: &Event) -> Option<String> {
    match event {
        Event::Measuring(stage) => Some(format!("{}…", label(*stage))),
        _ => None,
    }
}

struct Report<'a> {
    view: &'a View,
    run: &'a Run,
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
        Some(self.result(stage)?.status(self.run.focus.as_ref()))
    }

    /// What a planned stage shows without a value.
    fn unmeasured(&self, stage: Stage) -> &'static str {
        match self.status(stage) {
            None => "Skipped",
            Some(StageStatus::Complete) => MISSING,
            Some(StageStatus::Partial) => "Partial",
            Some(StageStatus::Failed) => "Failed",
            Some(StageStatus::Stopped) => "Stopped",
        }
    }

    /// The latency server's population in `stage`.
    fn population(&self, stage: Stage) -> Option<Population> {
        let result = self.result(stage)?;
        let server = result
            .servers
            .iter()
            .find(|server| Some(&server.server) == self.run.focus.as_ref());
        server?.latency
    }

    fn header(&self) -> Line {
        let mut facts = match self.view.servers.len() {
            0 => Vec::new(),
            1 => vec![self.view.servers[0].name.clone()],
            servers => vec![format!("{servers} servers")],
        };
        facts.push(clock(self.run.elapsed));
        let directions = self
            .run
            .results
            .iter()
            .flat_map(|result| [result.throughput.down, result.throughput.up]);
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
                let (value, mut facts) = match result {
                    Some((result, Some(throughput @ Throughput { rate: Some(rate), .. }))) => {
                        let facts = throughput_facts(throughput, result.measured, direction);
                        (Line::styled(format::rate(rate.mean), hue.bold()), facts)
                    }
                    Some((_, Some(_))) => (Line::styled(MISSING, palette.muted), Vec::new()),
                    _ => (Line::styled(self.unmeasured(stage), palette.muted), Vec::new()),
                };
                let partial = self.status(stage) == Some(StageStatus::Partial);
                if partial {
                    facts.insert(0, "Partial".into());
                }
                rows.push((named, value, facts, partial));
            }
        }
        let label_width = rows.iter().map(|row| row.0.width()).max().unwrap_or(0);
        let value_width = rows.iter().map(|row| row.1.width()).max().unwrap_or(0);
        let indent = label_width + value_width + 2;
        let mut lines = Vec::new();
        for (label, value, facts, partial) in rows {
            let first = label
                .pad(label_width)
                .and("  ", Style::default())
                .with(value.pad(value_width));
            let wrapped = wrap(&facts, self.width.saturating_sub(indent + 3).max(20));
            for (index, part) in wrapped.into_iter().enumerate() {
                let line = if index == 0 { first.clone() } else { Line::plain(" ".repeat(indent)) };
                lines.push(match part.strip_prefix("Partial").filter(|_| index == 0 && partial) {
                    Some(rest) => line
                        .and("   ", Style::default())
                        .and("Partial", palette.warn)
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
        let idle = self
            .population(Stage::Latency)
            .and_then(|population| population.median());
        let (mut rows, mut failures, mut added_shown, mut measured) = (Vec::new(), Vec::new(), false, false);
        for &(stage, _) in &self.run.plan {
            let result = self.result(stage);
            measured |= result.is_some() && !stage.directions().is_empty();
            for &direction in stage.directions() {
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
            failures.extend(result.and_then(|result| self.latency_failure(result, stage)));
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
        Some((self.grid(&headers, rows), failures, added_shown))
    }

    /// Why a direction has no result, or that it stopped.
    fn throughput_failure(&self, result: &StageResult, direction: Direction) -> Option<Line> {
        let label = direction_label(result.stage, direction);
        let throughput = result.throughput[direction]?;
        if result.stopped {
            return Some(Line::styled(format!("{label} stopped."), self.palette.warn));
        }
        if throughput.rate.is_some() {
            return None;
        }
        let failures: Vec<_> = result
            .failures
            .iter()
            .filter(|failure| failure.scope != Scope::Latency)
            .collect();
        let left = |server: &ServerResult| failures.iter().any(|failure| failure.server == server.server);
        let reason = match failures.last() {
            Some(failure) if result.servers.iter().all(left) => failure.failure.reason,
            _ => FailureReason::InsufficientEvidence,
        };
        Some(Line::styled(format!("{label}: {}", reason.label()), self.palette.err))
    }

    /// The latency server's latency failure in `stage`, or that it stopped.
    fn latency_failure(&self, result: &StageResult, stage: Stage) -> Option<Line> {
        let label = population_label(stage);
        if result.stopped {
            return Some(Line::styled(format!("{label} stopped."), self.palette.warn));
        }
        let focus = self.run.focus.as_ref()?;
        let failed = result
            .failures
            .iter()
            .find(|failure| failure.scope == Scope::Latency && failure.server == *focus);
        Some(Line::styled(format!("{label}: {}", failed?.failure.reason.label()), self.palette.err))
    }

    /// Latency populations with issues, the server's share of the round trips, and what Added means.
    fn notes(&self, added_shown: bool) -> Vec<Line> {
        let (mut notes, mut timing) = (Vec::new(), Vec::new());
        for &(stage, _) in &self.run.plan {
            let Some(population) = self.population(stage) else { continue };
            let summary = population.summary;
            let failed = self
                .result(stage)
                .and_then(|result| self.latency_failure(result, stage))
                .is_some();
            if failed || summary.timeouts > 0 || summary.unresolved > 0 || summary.send_failures > 0 {
                let mut facts = vec![format!("{} replies", count(summary.replies))];
                facts.extend(self.result(stage).map(|result| clock(result.measured)));
                facts.extend(
                    (summary.unresolved > 0).then(|| format!("unfinished probes {}", count(summary.unresolved))),
                );
                facts.extend(
                    (summary.send_failures > 0).then(|| format!("failed sends {}", count(summary.send_failures))),
                );
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
        lines
            .into_iter()
            .map(|line| Line::styled(line, self.palette.muted))
            .collect()
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
        let based = |cell: Line, base: Style| {
            Line(
                cell.0
                    .into_iter()
                    .map(|mut span| {
                        span.style = if span.style == Style::default() { base } else { span.style };
                        span
                    })
                    .collect(),
            )
        };
        if widths.iter().map(|width| width + 2).sum::<usize>().saturating_sub(2) <= self.width {
            let header = headers.iter().map(|header| Line::styled(*header, muted)).collect();
            let rows = std::iter::once(header).chain(
                rows.into_iter()
                    .map(|row| row.into_iter().map(|cell| based(cell, text)).collect()),
            );
            let joined = |row: Vec<Line>| {
                let cells = row.into_iter().zip(&widths).map(|(cell, width)| cell.pad(*width));
                cells
                    .reduce(|line, cell| line.and("  ", Style::default()).with(cell))
                    .unwrap_or_default()
                    .trimmed()
            };
            return rows.map(joined).collect();
        }
        let mut lines = vec![Line::styled(headers[0], muted)];
        for row in rows {
            let facts = row[1..].iter().zip(&headers[1..]).filter(|(cell, _)| cell.width() > 0);
            let facts: Vec<_> = facts
                .map(|(cell, header)| format!("{header} {}", cell.text()).trim().to_owned())
                .collect();
            lines.push(based(row[0].clone(), text));
            lines.extend(
                wrap(&facts, self.width.saturating_sub(2))
                    .into_iter()
                    .map(|fact| Line::plain("  ").and(fact, muted)),
            );
        }
        lines
    }
}
