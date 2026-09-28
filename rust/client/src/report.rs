use crate::{
    model::{Ending, FailureScope, Phase, ServerLatencyResult, Snapshot, Stage, StageResult, StageStatus},
    vocabulary::MISSING,
};
use graphite_meter_core::{failure::FailureReason, format, measurement::MeasurementResult};
use std::time::Duration;

pub const WIDTH: usize = 100;
const ADDED_NOTE: &str = "Added: loaded median minus idle median, same server.";

pub fn render(snapshot: &Snapshot, width: usize) -> Option<String> {
    if snapshot.participants.is_empty() && snapshot.results.is_empty() {
        return None;
    }
    let report = Report::new(snapshot, snapshot.latency_focus.as_deref(), width);
    let heading = match snapshot.latency_focus.as_deref() {
        Some(focus) if report.servers().len() > 1 => format!("Latency to {}", report.name(focus)),
        _ => "Latency".to_owned(),
    };
    let (latency, failures, added) = report.latency(heading);
    let mut blocks = vec![report.header()];
    if report.measured() {
        blocks.extend([report.throughput(), latency, failures.join("\n"), report.notes(added)]);
    }
    if report.servers().len() > 1 {
        blocks.push(report.details(false));
    }
    blocks.extend(snapshot.error.clone());
    let text = blocks
        .into_iter()
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n"))
}

pub fn results(snapshot: &Snapshot, shown: Option<&str>, width: usize) -> (Vec<Vec<String>>, Vec<String>) {
    let report = Report::new(snapshot, shown, width);
    if !report.measured() {
        return Default::default();
    }
    let (latency, failures, _) = report.latency("Latency".to_owned());
    let grids = [report.rates(), latency]
        .iter()
        .filter(|grid| !grid.is_empty())
        .map(|grid| grid.lines().map(str::to_owned).collect())
        .collect();
    (grids, failures)
}

pub fn label_stage(label: &str) -> Option<Stage> {
    [Stage::Latency, Stage::Download, Stage::Upload, Stage::Bidirectional]
        .into_iter()
        .find(|stage| label == compact_stage(*stage) || label == compact_population(*stage))
}

pub fn details(snapshot: &Snapshot, shown: Option<&str>, width: usize) -> String {
    Report::new(snapshot, shown, width).details(true)
}

pub fn status(snapshot: &Snapshot) -> &'static str {
    match snapshot.phase {
        Phase::Setup | Phase::Checking => "Not started",
        Phase::Warmup => "Warmup",
        Phase::Measuring => snapshot.stage.map_or("Checking paths", Stage::name),
        Phase::Preparing => "Checking paths",
        phase => outcome(phase),
    }
}

struct Report<'a> {
    snapshot: &'a Snapshot,
    plan: &'a [Stage],
    shown: Option<&'a str>,
    width: usize,
}

impl<'a> Report<'a> {
    fn new(snapshot: &'a Snapshot, shown: Option<&'a str>, width: usize) -> Self {
        Self {
            snapshot,
            plan: &snapshot.plan,
            shown,
            width,
        }
    }

    fn servers(&self) -> Vec<&crate::model::ServerSummary> {
        self.snapshot
            .servers
            .iter()
            .filter(|server| server.has_check_result())
            .collect()
    }

    fn name<'b>(&'b self, id: &'b str) -> &'b str {
        self.snapshot
            .servers
            .iter()
            .find(|server| server.id == id)
            .map_or(id, |server| server.name.as_str())
    }

    fn result(&self, stage: Stage) -> Option<&StageResult> {
        self.snapshot.results.iter().rfind(|result| result.stage == stage)
    }

    fn status(&self, stage: Stage) -> StageStatus {
        self.result(stage)
            .map_or(StageStatus::Skipped, |result| self.snapshot.stage_status(result))
    }

    fn unmeasured(&self, stage: Stage) -> &'static str {
        match self.status(stage) {
            StageStatus::Complete => MISSING,
            status => status.label(),
        }
    }

    fn measured(&self) -> bool {
        self.snapshot
            .results
            .iter()
            .any(|result| result.down.is_some() || result.up.is_some())
            || self.plan.iter().any(|stage| self.population(*stage).is_some())
    }

    fn header(&self) -> String {
        let servers = self.servers();
        let mut facts = Vec::new();
        match servers.as_slice() {
            [] => {}
            [server] => facts.push(server.name.clone()),
            servers => facts.push(format!("{} servers", servers.len())),
        }
        facts.push(clock(self.snapshot.duration));
        let total: u64 = self
            .snapshot
            .results
            .iter()
            .map(|result| result.down_bytes() + result.up_bytes())
            .sum();
        if total > 0 {
            facts.push(format::bytes(total));
        }
        format!(
            "Graphite Meter  {}  {}",
            outcome(self.snapshot.phase),
            facts.join(" · ")
        )
    }

    fn directions(&self) -> Vec<(Stage, Direction)> {
        self.plan
            .iter()
            .flat_map(|stage| {
                [
                    stage.downloads().then_some((*stage, Direction::Down)),
                    stage.uploads().then_some((*stage, Direction::Up)),
                ]
            })
            .flatten()
            .collect()
    }

    fn measurement(&self, stage: Stage, direction: Direction) -> Option<&MeasurementResult> {
        let result = self.result(stage)?;
        match direction {
            Direction::Down => result.down.as_ref(),
            Direction::Up => result.up.as_ref(),
        }
    }

    fn throughput(&self) -> String {
        let rows: Vec<_> = self
            .directions()
            .into_iter()
            .map(|(stage, direction)| {
                let label = format!("{} {}", direction.arrow(), stage.name());
                let (value, mut facts) = match self.measurement(stage, direction) {
                    None => (self.unmeasured(stage).to_owned(), Vec::new()),
                    Some(measurement) => match measurement.mean_bytes_per_sec {
                        None => (MISSING.to_owned(), Vec::new()),
                        Some(mean) => (format::rate(mean), throughput_facts(measurement, true)),
                    },
                };
                if self.status(stage) == StageStatus::Partial {
                    facts.insert(0, StageStatus::Partial.label().to_owned());
                }
                (label, value, facts)
            })
            .collect();
        let label_width = rows.iter().map(|row| width(&row.0)).max().unwrap_or(0);
        let value_width = rows.iter().map(|row| width(&row.1)).max().unwrap_or(0);
        let indent = label_width + value_width + 2;
        let mut lines = Vec::new();
        for (label, value, facts) in rows {
            let line = format!("{}  {}", pad(&label, label_width), pad(&value, value_width));
            for (index, part) in wrap_parts(&facts, self.width.saturating_sub(indent + 3).max(20))
                .into_iter()
                .enumerate()
            {
                lines.push(match index {
                    0 if part.is_empty() => line.clone(),
                    0 => format!("{line}   {part}"),
                    _ => format!("{}   {part}", " ".repeat(indent)),
                });
            }
        }
        lines.join("\n")
    }

    fn rates(&self) -> String {
        let mut rows = Vec::new();
        for stage in self.plan.iter().filter(|stage| stage.downloads() || stage.uploads()) {
            let rates: Vec<_> = self
                .directions()
                .into_iter()
                .filter(|(direction_stage, _)| direction_stage == stage)
                .filter_map(|(_, direction)| {
                    let measurement = self.measurement(*stage, direction)?;
                    let rate = measurement.mean_bytes_per_sec.map_or(MISSING.to_owned(), format::rate);
                    Some(format!("{} {rate}", direction.arrow()))
                })
                .collect();
            let mut rates = rates.join("  ");
            if rates.is_empty() && !self.snapshot.phase.live() {
                rates = self.unmeasured(*stage).to_owned();
            } else if self.status(*stage) == StageStatus::Partial {
                rates = format!("{rates}  {}", StageStatus::Partial.label());
            }
            if !rates.is_empty() {
                rows.push(vec![compact_stage(*stage).to_owned(), rates]);
            }
        }
        if rows.is_empty() {
            return String::new();
        }
        let scope = if self.servers().len() > 1 { "All servers" } else { "" };
        grid(&["Throughput".to_owned(), scope.to_owned()], &rows, self.width)
    }

    fn population(&self, stage: Stage) -> Option<&ServerLatencyResult> {
        let shown = self.shown?;
        self.result(stage)?
            .server_latencies
            .iter()
            .find(|host| host.id == shown)
    }

    fn latency(&self, heading: String) -> (String, Vec<String>, bool) {
        let idle = self.population(Stage::Latency).and_then(ServerLatencyResult::median);
        let mut rows = Vec::new();
        let mut failures = Vec::new();
        for stage in self.plan {
            for (direction_stage, direction) in self.directions() {
                if direction_stage != *stage {
                    continue;
                }
                let label = direction.label(*stage);
                if let Some(line) = self.throughput_failure(*stage, direction, &label) {
                    failures.push(line);
                }
            }
            match self.population(*stage) {
                Some(population) => {
                    let mut cells = latency_cells(population, idle);
                    if *stage == Stage::Latency {
                        cells[1].clear();
                    }
                    rows.push([vec![compact_population(*stage).to_owned()], cells].concat());
                    let label = population_label(*stage);
                    match population.ending {
                        Some(Ending::Stopped) if self.snapshot.phase == Phase::Cancelled => {
                            failures.push(format!("{label} stopped."));
                        }
                        Some(Ending::Failed(reason)) => failures.push(format!("{label}: {}", reason.label())),
                        _ => {}
                    }
                }
                None if *stage == Stage::Latency && !self.snapshot.phase.live() => {
                    rows.push(vec![
                        compact_population(*stage).to_owned(),
                        self.unmeasured(*stage).to_owned(),
                    ]);
                }
                None => {}
            }
        }
        let added = rows.iter().any(|row| row.get(2).is_some_and(|cell| !cell.is_empty()));
        if rows.is_empty() {
            return (String::new(), failures, added);
        }
        let mut headers = vec![heading, "Median".into(), "Added".into(), "P95".into(), "Jitter".into()];
        headers.push("Probe timeouts".into());
        for row in &mut rows {
            row.resize(headers.len(), String::new());
            if !added {
                row.remove(2);
            }
        }
        if !added {
            headers.remove(2);
        }
        (grid(&headers, &rows, self.width), failures, added)
    }

    fn throughput_failure(&self, stage: Stage, direction: Direction, label: &str) -> Option<String> {
        let result = self.result(stage)?;
        let measurement = self.measurement(stage, direction)?;
        if result.stopped {
            return Some(format!("{label} stopped."));
        }
        let stage_failures: Vec<_> = self
            .snapshot
            .failures
            .iter()
            .filter(|failure| failure.stage == stage && failure.scope == FailureScope::Throughput)
            .collect();
        let all_left = !result.server_results.is_empty()
            && result
                .server_results
                .iter()
                .all(|server| stage_failures.iter().any(|failure| failure.server_id == server.id));
        let reason = match stage_failures.last() {
            Some(failure) if all_left => failure.reason,
            _ if measurement.mean_bytes_per_sec.is_none() => FailureReason::InsufficientEvidence,
            _ => return None,
        };
        Some(format!("{label}: {}", reason.label()))
    }

    fn notes(&self, added: bool) -> String {
        let mut lines = Vec::new();
        let mut timing = Vec::new();
        for stage in self.plan {
            let Some(population) = self.population(*stage) else {
                continue;
            };
            let summary = population.summary;
            if population.ending.is_some()
                || summary.timeouts > 0
                || summary.unresolved > 0
                || summary.send_failures > 0
            {
                lines.extend(note(&population_label(*stage), &latency_facts(population), self.width));
            }
            if let Some(reflector) = summary.reflector_timing {
                let mut part = format!(
                    "{} {} of {}",
                    compact_population(*stage),
                    ms(reflector.mean_handling),
                    ms(reflector.mean_raw_rtt)
                );
                if reflector.count != summary.count {
                    part.push_str(&format!(" ({} pairs)", count(reflector.count)));
                }
                timing.push(part);
            }
        }
        if !timing.is_empty() {
            lines.extend(note("Server handling of the mean round trip", &timing, self.width));
        }
        if added {
            lines.push(ADDED_NOTE.to_owned());
        }
        lines.join("\n")
    }

    fn details(&self, full: bool) -> String {
        let servers = self.servers();
        let remaining = self.snapshot.participants.len();
        let outcome = outcome(self.snapshot.phase);
        let live = self.snapshot.phase.live();
        let mut lines = vec![match servers.len() {
            1 => status(self.snapshot).to_owned(),
            selected if live && remaining < selected => format!("{remaining} of {selected} servers remaining"),
            selected if live => format!("All {selected} servers"),
            selected if remaining < selected => format!("{outcome} · {remaining} of {selected} servers"),
            selected => format!("{outcome} · all {selected} servers"),
        }];
        if full {
            let notes = self.facts();
            if !notes.is_empty() {
                lines.extend(notes);
                lines.push(String::new());
            }
        }
        let directions = self.directions();
        let mut headers = vec!["Server".to_owned()];
        headers.extend(directions.iter().map(|(stage, direction)| direction.label(*stage)));
        let cell = |measurement: Option<&MeasurementResult>| {
            measurement
                .and_then(|measurement| measurement.mean_bytes_per_sec)
                .map_or_else(|| MISSING.to_owned(), format::rate)
        };
        let mut rows = vec![
            std::iter::once("All servers".to_owned())
                .chain(
                    directions
                        .iter()
                        .map(|(stage, direction)| cell(self.measurement(*stage, *direction))),
                )
                .collect::<Vec<_>>(),
        ];
        let mut medians = Vec::new();
        for server in &servers {
            let mut name = server.name.clone();
            if !self.snapshot.participants.contains(&server.id) {
                name.push_str(" ✗");
            }
            let own = |stage: Stage, direction: Direction| {
                let contribution = self
                    .result(stage)?
                    .server_results
                    .iter()
                    .find(|contribution| contribution.id == server.id)?;
                match direction {
                    Direction::Down => contribution.down.as_ref(),
                    Direction::Up => contribution.up.as_ref(),
                }
            };
            rows.push(
                std::iter::once(name)
                    .chain(
                        directions
                            .iter()
                            .map(|(stage, direction)| cell(own(*stage, *direction))),
                    )
                    .collect(),
            );
            let median = |stage: &Stage| {
                self.result(*stage)
                    .and_then(|result| result.server_latencies.iter().find(|host| host.id == server.id))
                    .and_then(ServerLatencyResult::median)
                    .map_or_else(|| MISSING.to_owned(), ms)
            };
            medians.push(
                std::iter::once(server.name.clone())
                    .chain(self.plan.iter().map(median))
                    .collect::<Vec<_>>(),
            );
        }
        lines.push(grid(&headers, &rows, self.width));
        lines.extend([String::new(), "Latency median by server".to_owned()]);
        let populations: Vec<_> = std::iter::once("Server".to_owned())
            .chain(self.plan.iter().map(|stage| compact_population(*stage).to_owned()))
            .collect();
        lines.push(grid(&populations, &medians, self.width));
        if !self.snapshot.failures.is_empty() {
            lines.extend([String::new(), "Issues".to_owned()]);
            for failure in &self.snapshot.failures {
                let scope = match failure.scope {
                    FailureScope::Throughput => "throughput",
                    FailureScope::Latency => "latency",
                };
                lines.push(format!(
                    "{} · {} {scope} · at {} · {}",
                    self.name(&failure.server_id),
                    compact_stage(failure.stage),
                    clock(failure.at),
                    failure.reason.label()
                ));
            }
        }
        let intervals: Vec<_> = self
            .snapshot
            .results
            .iter()
            .flat_map(|result| result.intervals.iter().map(move |interval| (result.stage, interval)))
            .collect();
        if full && !live && !intervals.is_empty() {
            lines.extend([String::new(), "Aggregation intervals".to_owned()]);
            for (stage, interval) in intervals {
                let names: Vec<_> = interval.participants.iter().map(|id| self.name(id)).collect();
                let state = if interval.complete && interval.window.is_some() {
                    "measured window"
                } else {
                    "incomplete evidence"
                };
                let parts = [
                    format!(
                        "{} {:.1}–{:.1} s",
                        compact_stage(stage),
                        interval.start_nanos as f64 / 1e9,
                        interval.end_nanos as f64 / 1e9
                    ),
                    names.join(", "),
                    state.to_owned(),
                ];
                lines.push(
                    parts
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · "),
                );
            }
            let omitted: usize = self
                .snapshot
                .results
                .iter()
                .map(|result| result.omitted_intervals)
                .sum();
            if omitted > 0 {
                lines.push(format!(
                    "{omitted} older intervals omitted; byte totals retain the full run"
                ));
            }
        }
        let text = lines.join("\n");
        text.lines()
            .map(|line| fit(line, self.width))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn facts(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for stage in self.plan {
            for (direction_stage, direction) in self.directions() {
                let Some(measurement) = self
                    .measurement(*stage, direction)
                    .filter(|_| direction_stage == *stage)
                else {
                    continue;
                };
                if measurement.mean_bytes_per_sec.is_some() || measurement.total_bytes > 0 {
                    let facts = throughput_facts(measurement, false);
                    notes.extend(note(&direction.label(*stage), &facts, self.width));
                }
            }
            let Some(population) = self.population(*stage) else {
                continue;
            };
            notes.extend(note(&population_label(*stage), &latency_facts(population), self.width));
            if let Some(timing) = population.summary.reflector_timing {
                notes.extend(note(
                    &format!("Server timing ({} paired replies, means)", count(timing.count)),
                    &[
                        format!("raw {}", ms(timing.mean_raw_rtt)),
                        format!("handling {}", ms(timing.mean_handling)),
                    ],
                    self.width,
                ));
            }
        }
        notes
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Down,
    Up,
}

impl Direction {
    fn arrow(self) -> &'static str {
        match self {
            Self::Down => "↓",
            Self::Up => "↑",
        }
    }

    fn label(self, stage: Stage) -> String {
        match stage {
            Stage::Bidirectional => format!("Bi-dir {}", self.arrow()),
            stage => stage.name().to_owned(),
        }
    }
}

fn outcome(phase: Phase) -> &'static str {
    match phase {
        Phase::Complete => "Complete",
        Phase::Partial => "Partial",
        Phase::Incomplete => "Incomplete",
        Phase::Cancelled => "Stopped",
        _ => "Failed",
    }
}

fn compact_stage(stage: Stage) -> &'static str {
    match stage {
        Stage::Bidirectional => "Bi-dir",
        stage => stage.name(),
    }
}

fn population_label(stage: Stage) -> String {
    match stage {
        Stage::Latency => "Idle latency".to_owned(),
        stage => format!("Loaded latency · {}", stage.name()),
    }
}

fn compact_population(stage: Stage) -> &'static str {
    match stage {
        Stage::Latency => "Idle",
        Stage::Download => "Loaded down",
        Stage::Upload => "Loaded up",
        Stage::Bidirectional => "Loaded bi-dir",
    }
}

fn latency_cells(population: &ServerLatencyResult, idle: Option<u64>) -> Vec<String> {
    let summary = population.summary;
    let mut cells = vec![MISSING.to_owned(); 5];
    if let Some(median) = population.median() {
        cells[0] = ms(median);
        if let Some(idle) = idle {
            cells[1] = format!("{} ms", format::added_ms((median as f64 - idle as f64) / 1e6));
        }
    }
    if let Some(distribution) = summary.distribution {
        cells[2] = ms(distribution.p95);
    }
    if let Some(jitter) = summary.jitter {
        cells[3] = ms(jitter);
    }
    if let Some(ratio) = summary.timeout_ratio() {
        cells[4] = format!(
            "{} / {}",
            count(summary.timeouts),
            count(summary.count + summary.timeouts)
        );
        if ratio >= 0.01 {
            cells[4].push_str(&format!(" ({:.1}%)", ratio * 100.0));
        } else if ratio > 0.0 {
            cells[4].push_str(&format!(" ({:.2}%)", ratio * 100.0));
        }
    }
    cells
}

fn latency_facts(population: &ServerLatencyResult) -> Vec<String> {
    let summary = population.summary;
    let mut facts = vec![format!("{} replies", count(summary.count))];
    if let Some(elapsed) = population.elapsed.filter(|elapsed| !elapsed.is_zero()) {
        facts.push(clock(elapsed));
    }
    if summary.unresolved > 0 {
        facts.push(format!("unfinished probes {}", count(summary.unresolved)));
    }
    if summary.send_failures > 0 {
        facts.push(format!("failed sends {}", count(summary.send_failures)));
    }
    facts
}

fn throughput_facts(measurement: &MeasurementResult, brief: bool) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(peak) = measurement.peak_bytes_per_sec.filter(|peak| *peak > 0.0) {
        let peak = format::rate(peak);
        let unit = measurement
            .mean_bytes_per_sec
            .filter(|_| brief)
            .and_then(|mean| Some(format!(" {}", format::rate(mean).rsplit_once(' ')?.1)));
        let peak = unit.and_then(|unit| peak.strip_suffix(&unit)).unwrap_or(&peak);
        facts.push(format!("peak {peak}"));
    }
    facts.push(format::bytes(measurement.total_bytes));
    if let Some(elapsed) = measurement.elapsed_nanos.filter(|elapsed| *elapsed > 0) {
        facts.push(clock(Duration::from_nanos(elapsed)));
    }
    if measurement.samples > 0 && !brief {
        facts.push(format!("{} samples", count(measurement.samples)));
    }
    if measurement.direction == graphite_meter_core::measurement::Direction::Up {
        facts.push("receiver-timed".to_owned());
    }
    facts
}

fn note(label: &str, facts: &[String], width: usize) -> Vec<String> {
    let first = format!("{label}: {}", facts[0]);
    let (mut lines, facts) = if self::width(&first) <= width.saturating_sub(2) {
        (Vec::new(), [vec![first], facts[1..].to_vec()].concat())
    } else {
        (vec![format!("{label}:")], facts.to_vec())
    };
    for line in wrap_parts(&facts, width.saturating_sub(2)) {
        lines.push(if lines.is_empty() { line } else { format!("  {line}") });
    }
    lines
}

fn grid(headers: &[String], rows: &[Vec<String>], limit: usize) -> String {
    let mut widths = vec![0; headers.len()];
    for row in std::iter::once(headers).chain(rows.iter().map(Vec::as_slice)) {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(width(cell));
        }
    }
    if widths.iter().map(|width| width + 2).sum::<usize>().saturating_sub(2) <= limit {
        return std::iter::once(headers)
            .chain(rows.iter().map(Vec::as_slice))
            .map(|row| {
                let cells: Vec<_> = row.iter().zip(&widths).map(|(cell, width)| pad(cell, *width)).collect();
                cells.join("  ").trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let mut lines = vec![headers[0].clone()];
    for row in rows {
        let facts: Vec<_> = row[1..]
            .iter()
            .zip(&headers[1..])
            .filter(|(cell, _)| !cell.is_empty())
            .map(|(cell, header)| format!("{header} {cell}").trim().to_owned())
            .collect();
        lines.push(row[0].clone());
        lines.extend(
            wrap_parts(&facts, limit.saturating_sub(2))
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }
    lines.join("\n")
}

fn wrap_parts(parts: &[String], limit: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for part in parts {
        if line.is_empty() {
            line.clone_from(part);
        } else if line.chars().count() + 3 + part.chars().count() <= limit {
            line.push_str(" · ");
            line.push_str(part);
        } else {
            lines.push(std::mem::replace(&mut line, part.clone()));
        }
    }
    lines.push(line);
    lines
}

fn fit(line: &str, limit: usize) -> String {
    if width(line) <= limit {
        return line.to_owned();
    }
    let mut fitted = String::new();
    let mut used = 0;
    for character in line.chars() {
        used += unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used >= limit.max(1) {
            break;
        }
        fitted.push(character);
    }
    fitted + "…"
}

fn width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn pad(text: &str, to: usize) -> String {
    format!("{text}{}", " ".repeat(to.saturating_sub(width(text))))
}

fn ms(nanos: u64) -> String {
    format!("{} ms", format::latency_ms(nanos as f64 / 1e6))
}

fn clock(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}
