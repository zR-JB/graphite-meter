use crate::{
    model::{
        Ending, FailureScope, Phase, ServerFailure, ServerLatencyResult, Snapshot, Stage, StageResult, StageStatus,
    },
    theme::Theme,
    vocabulary::MISSING,
};
use graphite_meter_core::{failure::FailureReason, format, measurement::MeasurementResult, text::terminal_character};
use ratatui::style::Color;
use std::time::Duration;

pub const WIDTH: usize = 100;
const ADDED_NOTE: &str = "Added: loaded median minus idle median, same server.";
const RESET: &str = "\x1b[0m";

/// The final report, terminal-safe; a terminal gets Go's colours.
pub fn render(snapshot: &Snapshot, width: usize, terminal: bool) -> Option<String> {
    compose(snapshot, width, palette(terminal))
}

/// Go prints the report through lipgloss's colour profile: a terminal gets the TUI palette unless
/// TERM is dumb, and NO_COLOR keeps only bold, as the monochrome theme does.
fn palette(terminal: bool) -> Option<Theme> {
    let term = std::env::var("TERM").unwrap_or_default();
    let dumb = term == "dumb" || term.is_empty() && !cfg!(windows);
    (terminal && !dumb).then(Theme::terminal)
}

fn compose(snapshot: &Snapshot, width: usize, theme: Option<Theme>) -> Option<String> {
    if snapshot.participants.is_empty() && snapshot.results.is_empty() {
        return None;
    }
    let mut report = Report::new(snapshot, snapshot.latency_focus.as_deref(), width);
    report.theme = theme;
    let heading = match snapshot.latency_focus.as_deref() {
        Some(focus) if run_servers(snapshot).len() > 1 => format!("Latency to {}", server_name(snapshot, focus)),
        _ => "Latency".to_owned(),
    };
    let (latency, failures, added) = report.latency(heading);
    let mut blocks = vec![report.header()];
    if report.measured() {
        blocks.extend([report.throughput(), latency, failures.join("\n"), report.notes(added)]);
    }
    if run_servers(snapshot).len() > 1 {
        blocks.push(report.details(false));
    }
    blocks.extend(snapshot.error.as_deref().map(|error| report.paint(Tone::Err, error)));
    let text = blocks
        .into_iter()
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(
        text.lines()
            .map(|line| safe(line.trim_end()))
            .collect::<Vec<_>>()
            .join("\n"),
    )
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

/// The run's servers as Go's run details list them: every selected server the check reached.
pub(crate) fn run_servers(snapshot: &Snapshot) -> Vec<&crate::model::ServerSummary> {
    snapshot
        .servers
        .iter()
        .filter(|server| server.has_check_result())
        .collect()
}

/// A server's catalogue name, or its ID when the catalogue lacks it.
pub(crate) fn server_name<'a>(snapshot: &'a Snapshot, id: &'a str) -> &'a str {
    let server = snapshot.servers.iter().find(|server| server.id == id);
    server.map_or(id, |server| server.name.as_str())
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
    theme: Option<Theme>,
}

/// Go's report styles.
#[derive(Clone, Copy)]
enum Tone {
    Heading,
    Text,
    Muted,
    Warn,
    Err,
    Stage(Stage),
    Rate(Stage),
    Outcome(Phase),
}

impl<'a> Report<'a> {
    fn new(snapshot: &'a Snapshot, shown: Option<&'a str>, width: usize) -> Self {
        Self {
            snapshot,
            plan: &snapshot.plan,
            shown,
            width,
            theme: None,
        }
    }

    /// One line of text in Go's lipgloss style; the TUI's report stays plain.
    fn paint(&self, tone: Tone, text: &str) -> String {
        let Some(theme) = self.theme.filter(|_| !text.is_empty()) else {
            return text.to_owned();
        };
        let (color, bold) = match tone {
            Tone::Heading => (theme.ink, true),
            Tone::Text => (theme.text, false),
            Tone::Muted => (theme.muted, false),
            Tone::Warn => (theme.warn, false),
            Tone::Err => (theme.err, true),
            Tone::Stage(stage) => (theme.stage(stage), false),
            Tone::Rate(stage) => (theme.stage(stage), true),
            Tone::Outcome(Phase::Complete) => (theme.ok, true),
            Tone::Outcome(Phase::Partial | Phase::Incomplete | Phase::Cancelled) => (theme.warn, true),
            Tone::Outcome(_) => (theme.err, true),
        };
        let codes: Vec<_> = bold.then(|| "1".to_owned()).into_iter().chain(sgr(color)).collect();
        if codes.is_empty() {
            return text.to_owned();
        }
        format!("\x1b[{}m{text}{RESET}", codes.join(";"))
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
        let servers = run_servers(self.snapshot);
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
            "{}  {}  {}",
            self.paint(Tone::Heading, "Graphite Meter"),
            self.paint(Tone::Outcome(self.snapshot.phase), outcome(self.snapshot.phase)),
            self.paint(Tone::Muted, &facts.join(" · "))
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
                let label = format!(
                    "{} {}",
                    self.paint(Tone::Stage(stage), direction.arrow()),
                    self.paint(Tone::Text, stage.name())
                );
                let (value, mut facts) = match self.measurement(stage, direction) {
                    None => (self.paint(Tone::Muted, self.unmeasured(stage)), Vec::new()),
                    Some(measurement) => match measurement.mean_bytes_per_sec {
                        None => (self.paint(Tone::Muted, MISSING), Vec::new()),
                        Some(mean) => (
                            self.paint(Tone::Rate(stage), &format::rate(mean)),
                            throughput_facts(measurement, true),
                        ),
                    },
                };
                let partial = self.status(stage) == StageStatus::Partial;
                if partial {
                    facts.insert(0, StageStatus::Partial.label().to_owned());
                }
                (label, value, facts, partial)
            })
            .collect();
        let label_width = rows.iter().map(|row| width(&row.0)).max().unwrap_or(0);
        let value_width = rows.iter().map(|row| width(&row.1)).max().unwrap_or(0);
        let indent = label_width + value_width + 2;
        let mut lines = Vec::new();
        for (label, value, facts, partial) in rows {
            let line = format!("{}  {}", pad(&label, label_width), pad(&value, value_width));
            for (index, part) in wrap_parts(&facts, self.width.saturating_sub(indent + 3).max(20))
                .into_iter()
                .enumerate()
            {
                let status = StageStatus::Partial.label();
                lines.push(match index {
                    0 if part.is_empty() => line.clone(),
                    0 if partial => format!(
                        "{line}   {}{}",
                        self.paint(Tone::Warn, status),
                        self.paint(Tone::Muted, part.strip_prefix(status).unwrap_or(&part))
                    ),
                    0 => format!("{line}   {}", self.paint(Tone::Muted, &part)),
                    _ => format!("{}   {}", " ".repeat(indent), self.paint(Tone::Muted, &part)),
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
        let scope = if run_servers(self.snapshot).len() > 1 {
            "All servers"
        } else {
            ""
        };
        self.grid(&["Throughput".to_owned(), scope.to_owned()], &rows)
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
                    let label = self.paint(Tone::Stage(*stage), compact_population(*stage));
                    rows.push([vec![label], cells].concat());
                    let label = population_label(*stage);
                    match population.ending {
                        Some(Ending::Stopped) if self.snapshot.phase == Phase::Cancelled => {
                            failures.push(self.paint(Tone::Warn, &format!("{label} stopped.")));
                        }
                        Some(Ending::Failed(reason)) => {
                            failures.push(self.paint(Tone::Err, &format!("{label}: {}", reason.label())));
                        }
                        _ => {}
                    }
                }
                None if *stage == Stage::Latency && !self.snapshot.phase.live() => {
                    rows.push(vec![
                        self.paint(Tone::Stage(*stage), compact_population(*stage)),
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
        (self.grid(&headers, &rows), failures, added)
    }

    fn throughput_failure(&self, stage: Stage, direction: Direction, label: &str) -> Option<String> {
        let result = self.result(stage)?;
        let measurement = self.measurement(stage, direction)?;
        if result.stopped {
            return Some(self.paint(Tone::Warn, &format!("{label} stopped.")));
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
        Some(self.paint(Tone::Err, &format!("{label}: {}", reason.label())))
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
        let lines: Vec<_> = lines.iter().map(|line| self.paint(Tone::Muted, line)).collect();
        lines.join("\n")
    }

    /// Go's detailsView: the outcome, the facts in full, the rates and latency medians by server,
    /// the issues and, in full once the run ends, its aggregation intervals.
    fn details(&self, full: bool) -> String {
        let mut lines = vec![self.paint(Tone::Heading, &self.outcome_notice())];
        let facts = if full { self.facts() } else { Vec::new() };
        if !facts.is_empty() {
            lines.extend(facts);
            lines.push(String::new());
        }
        lines.push(self.server_rates());
        lines.extend([String::new(), self.paint(Tone::Heading, "Latency median by server")]);
        lines.push(self.server_medians());
        if !self.snapshot.failures.is_empty() {
            lines.extend([String::new(), self.paint(Tone::Heading, "Issues")]);
            lines.extend(self.snapshot.failures.iter().map(|failure| self.issue(failure)));
        }
        if full && !self.snapshot.phase.live() && !self.snapshot.intervals.is_empty() {
            lines.extend([String::new(), "Aggregation intervals".to_owned()]);
            lines.extend(self.intervals());
        }
        let text = lines.join("\n");
        text.lines()
            .map(|line| fit(line, self.width))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Go's outcomeNotice: one server's status, or how many of the run's servers remain.
    fn outcome_notice(&self) -> String {
        let (remaining, outcome) = (self.snapshot.participants.len(), outcome(self.snapshot.phase));
        let live = self.snapshot.phase.live();
        match run_servers(self.snapshot).len() {
            1 => status(self.snapshot).to_owned(),
            selected if live && remaining < selected => format!("{remaining} of {selected} servers remaining"),
            selected if live => format!("All {selected} servers"),
            selected if remaining < selected => format!("{outcome} · {remaining} of {selected} servers"),
            selected => format!("{outcome} · all {selected} servers"),
        }
    }

    /// Each direction's mean rate for the run, then for each server; one that left is marked ✗.
    fn server_rates(&self) -> String {
        let directions = self.directions();
        let rate = |measurement: Option<&MeasurementResult>| {
            measurement
                .and_then(|measurement| measurement.mean_bytes_per_sec)
                .map_or_else(|| MISSING.to_owned(), format::rate)
        };
        let mut headers = vec!["Server".to_owned()];
        headers.extend(directions.iter().map(|(stage, direction)| direction.label(*stage)));
        let mut all = vec!["All servers".to_owned()];
        all.extend(
            directions
                .iter()
                .map(|(stage, direction)| rate(self.measurement(*stage, *direction))),
        );
        let mut rows = vec![all];
        for server in run_servers(self.snapshot) {
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
            let mut row = vec![server.name.clone()];
            if !self.snapshot.participants.contains(&server.id) {
                row[0].push_str(" ✗");
            }
            row.extend(
                directions
                    .iter()
                    .map(|(stage, direction)| rate(own(*stage, *direction))),
            );
            rows.push(row);
        }
        self.grid(&headers, &rows)
    }

    /// Each server's latency median in each planned stage.
    fn server_medians(&self) -> String {
        let mut headers = vec!["Server".to_owned()];
        headers.extend(self.plan.iter().map(|stage| compact_population(*stage).to_owned()));
        let rows: Vec<_> = run_servers(self.snapshot)
            .into_iter()
            .map(|server| {
                let median = |stage: &Stage| {
                    self.result(*stage)
                        .and_then(|result| result.server_latencies.iter().find(|host| host.id == server.id))
                        .and_then(ServerLatencyResult::median)
                        .map_or_else(|| MISSING.to_owned(), ms)
                };
                std::iter::once(server.name.clone())
                    .chain(self.plan.iter().map(median))
                    .collect()
            })
            .collect();
        self.grid(&headers, &rows)
    }

    fn issue(&self, failure: &ServerFailure) -> String {
        let scope = match failure.scope {
            FailureScope::Throughput => "throughput",
            FailureScope::Latency => "latency",
        };
        format!(
            "{} · {} {scope} · at {} · {}",
            server_name(self.snapshot, &failure.server_id),
            compact_stage(failure.stage),
            clock(failure.at),
            failure.reason.label()
        )
    }

    /// Go's run details: one list of the run's aggregation intervals, timed from its start.
    fn intervals(&self) -> Vec<String> {
        let seconds = |nanos| Duration::from_nanos(nanos).as_secs_f64();
        let mut lines: Vec<_> = self
            .snapshot
            .intervals
            .iter()
            .map(|interval| {
                let names: Vec<_> = interval
                    .participants
                    .iter()
                    .map(|id| server_name(self.snapshot, id))
                    .collect();
                let state = if interval.complete && interval.window.is_some() {
                    "measured window"
                } else {
                    "incomplete evidence"
                };
                let (start, end) = (seconds(interval.start_nanos), seconds(interval.end_nanos));
                let span = format!("{} {start:.1}–{end:.1} s", compact_stage(interval.stage.into()));
                [span, names.join(", "), state.to_owned()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .collect();
        let omitted = self.snapshot.omitted_intervals;
        if omitted > 0 {
            lines.push(format!(
                "{omitted} older intervals omitted; byte totals retain the full run"
            ));
        }
        lines
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

impl Report<'_> {
    /// Go's grid: muted headers over text cells, or each row's facts when the columns do not fit.
    fn grid(&self, headers: &[String], rows: &[Vec<String>]) -> String {
        let limit = self.width;
        let mut widths = vec![0; headers.len()];
        for row in std::iter::once(headers).chain(rows.iter().map(Vec::as_slice)) {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(width(cell));
            }
        }
        if widths.iter().map(|width| width + 2).sum::<usize>().saturating_sub(2) <= limit {
            return std::iter::once(headers)
                .chain(rows.iter().map(Vec::as_slice))
                .enumerate()
                .map(|(index, row)| {
                    let tone = if index == 0 { Tone::Muted } else { Tone::Text };
                    let cells: Vec<_> = row
                        .iter()
                        .zip(&widths)
                        .map(|(cell, width)| pad(&self.paint(tone, cell), *width))
                        .collect();
                    cells.join("  ").trim_end().to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        let mut lines = vec![self.paint(Tone::Muted, &headers[0])];
        for row in rows {
            let facts: Vec<_> = row[1..]
                .iter()
                .zip(&headers[1..])
                .filter(|(cell, _)| !cell.is_empty())
                .map(|(cell, header)| format!("{header} {cell}").trim().to_owned())
                .collect();
            lines.push(self.paint(Tone::Text, &row[0]));
            lines.extend(
                wrap_parts(&facts, limit.saturating_sub(2))
                    .into_iter()
                    .map(|line| format!("  {}", self.paint(Tone::Muted, &line))),
            );
        }
        lines.join("\n")
    }
}

pub(crate) fn wrap_parts(parts: &[String], limit: usize) -> Vec<String> {
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
    let mut rest = line;
    while let Some(character) = rest.chars().next() {
        if let Some(length) = sgr_length(rest) {
            fitted.push_str(&rest[..length]);
            rest = &rest[length..];
            continue;
        }
        used += unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used >= limit.max(1) {
            break;
        }
        fitted.push(character);
        rest = &rest[character.len_utf8()..];
    }
    fitted.push('…');
    if fitted.contains('\x1b') {
        fitted.push_str(RESET);
    }
    fitted
}

/// Display width; the SGR sequences `paint` adds take none.
fn width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(unpainted(text).as_str())
}

fn unpainted(text: &str) -> String {
    map_text(text, false, |character| character)
}

/// The length of the SGR sequence `text` starts with, as `paint` writes them.
fn sgr_length(text: &str) -> Option<usize> {
    let parameters = text.strip_prefix("\x1b[")?;
    let end = parameters.find(|c: char| !c.is_ascii_digit() && c != ';')?;
    parameters[end..].starts_with('m').then_some(end + 3)
}

/// The TUI's safe text for a report line, keeping only the SGR sequences `paint` adds.
fn safe(line: &str) -> String {
    map_text(line, true, terminal_char)
}

/// The text with each character mapped, keeping or dropping the SGR sequences `paint` adds.
fn map_text(text: &str, keep_sgr: bool, map: fn(char) -> char) -> String {
    let mut mapped = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(character) = rest.chars().next() {
        let length = match sgr_length(rest) {
            Some(length) if keep_sgr => {
                mapped.push_str(&rest[..length]);
                length
            }
            Some(length) => length,
            None => {
                mapped.push(map(character));
                character.len_utf8()
            }
        };
        rest = &rest[length..];
    }
    mapped
}

/// A character the terminal shows as itself, or the replacement for one that could control it.
pub(crate) fn terminal_char(c: char) -> char {
    if terminal_character(c) { c } else { '�' }
}

/// A foreground colour's SGR parameters, as Go's colour profiles write each depth.
fn sgr(color: Color) -> Option<String> {
    let ansi = match color {
        Color::Reset => return None,
        Color::Rgb(red, green, blue) => return Some(format!("38;2;{red};{green};{blue}")),
        Color::Indexed(index) => return Some(format!("38;5;{index}")),
        Color::Black => 30,
        Color::Red => 31,
        Color::Green => 32,
        Color::Yellow => 33,
        Color::Blue => 34,
        Color::Magenta => 35,
        Color::Cyan => 36,
        Color::Gray => 37,
        Color::DarkGray => 90,
        Color::LightRed => 91,
        Color::LightGreen => 92,
        Color::LightYellow => 93,
        Color::LightBlue => 94,
        Color::LightMagenta => 95,
        Color::LightCyan => 96,
        Color::White => 97,
    };
    Some(ansi.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ServerContribution, ServerFailure, ServerSummary};
    use crate::ui::tests::{download_measurement, latency_result, probes};

    /// One server's idle and loaded latency and a download a late probe timeout made partial.
    fn partial_run() -> Snapshot {
        let latency = |rtts: &[i64], timeouts| vec![latency_result("a", probes(rtts, timeouts))];
        Snapshot {
            phase: Phase::Partial,
            servers: vec![ServerSummary {
                id: "a".into(),
                name: "Alpha".into(),
                error: Some("checked".into()),
                ..ServerSummary::default()
            }],
            participants: vec!["a".into()],
            latency_focus: Some("a".into()),
            plan: vec![Stage::Latency, Stage::Download],
            duration: Duration::from_secs(5),
            results: vec![
                StageResult {
                    stage: Stage::Latency,
                    elapsed: Duration::from_secs(1),
                    server_latencies: latency(&[1_000_000, 2_000_000, 3_000_000], 1),
                    ..StageResult::default()
                },
                StageResult {
                    stage: Stage::Download,
                    elapsed: Duration::from_secs(1),
                    down: Some(MeasurementResult {
                        peak_bytes_per_sec: Some(1_800_000.0),
                        ..download_measurement()
                    }),
                    server_latencies: latency(&[5_000_000, 6_000_000, 7_000_000], 0),
                    server_results: vec![ServerContribution {
                        id: "a".into(),
                        ..ServerContribution::default()
                    }],
                    ..StageResult::default()
                },
            ],
            failures: vec![ServerFailure {
                server_id: "a".into(),
                stage: Stage::Download,
                scope: FailureScope::Latency,
                reason: FailureReason::Timeout,
                at: Duration::from_secs(3),
            }],
            error: Some("Stopped delivering data".into()),
            ..Snapshot::default()
        }
    }

    #[test]
    fn a_terminal_report_paints_go_styles_over_the_plain_text() {
        let snapshot = partial_run();
        let plain = compose(&snapshot, WIDTH, None).unwrap();
        assert!(!plain.contains('\x1b'));
        assert_eq!(render(&snapshot, WIDTH, false).as_ref(), Some(&plain));
        let mut theme = Theme::terminal();
        (theme.ink, theme.text, theme.muted) = (Color::Indexed(1), Color::Indexed(2), Color::Indexed(3));
        (theme.warn, theme.err) = (Color::Indexed(4), Color::Rgb(5, 6, 7));
        let painted = compose(&snapshot, WIDTH, Some(theme)).unwrap();
        assert_eq!(unpainted(&painted), plain);
        let report = Report {
            theme: Some(theme),
            ..Report::new(&snapshot, None, WIDTH)
        };
        let paint = |tone, text: &str| report.paint(tone, text);
        let download = Stage::Download;
        for line in [
            "\x1b[1;38;5;1mGraphite Meter\x1b[0m  \x1b[1;38;5;4mPartial\x1b[0m  \x1b[38;5;3mAlpha · 5.0 s · 1.5 MB\x1b[0m"
                .to_owned(),
            format!(
                "{} {}  {}   {}{}",
                paint(Tone::Stage(download), "↓"),
                paint(Tone::Text, "Download"),
                paint(Tone::Rate(download), "12.00 Mbit/s"),
                paint(Tone::Warn, "Partial"),
                paint(Tone::Muted, " · peak 14.40 · 1.5 MB · 1.0 s")
            ),
            format!("{}      {}", paint(Tone::Muted, "Latency"), paint(Tone::Muted, "Median")),
            format!(
                "{}  {}",
                paint(Tone::Text, &paint(Tone::Stage(download), "Loaded down")),
                paint(Tone::Text, "6.0 ms")
            ),
            paint(Tone::Muted, ADDED_NOTE),
            "\x1b[1;38;2;5;6;7mStopped delivering data\x1b[0m".to_owned(),
        ] {
            assert!(painted.lines().any(|painted| painted.contains(&line)), "{line:?} in {painted:?}");
        }

        // A multi-server run's details keep Go's headings; its issues stay plain.
        let mut pair = partial_run();
        pair.servers.push(ServerSummary {
            id: "b".into(),
            name: "Beta".into(),
            error: Some("checked".into()),
            ..ServerSummary::default()
        });
        let painted = compose(&pair, WIDTH, Some(theme)).unwrap();
        assert_eq!(unpainted(&painted), compose(&pair, WIDTH, None).unwrap());
        for line in [
            paint(Tone::Heading, "Partial · 1 of 2 servers"),
            paint(Tone::Heading, "Latency median by server"),
            paint(Tone::Heading, "Issues"),
            "Alpha · Download latency · at 3.0 s · Stopped delivering data".to_owned(),
        ] {
            assert!(
                painted.lines().any(|painted| painted == line),
                "{line:?} in {painted:?}"
            );
        }
    }

    #[test]
    fn painted_lines_measure_fit_and_sanitize_around_their_colours() {
        let name = "\x1b[1;38;5;1mname\x1b[0m";
        assert_eq!(width(name), 4);
        assert_eq!(pad(name, 6), format!("{name}  "));
        assert_eq!(fit("\x1b[38;5;3mabcdef\x1b[0m", 4), "\x1b[38;5;3mabc…\x1b[0m");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(
            safe(&format!("{name}\x1b]52;c;secret\x07\u{202e}\x1b[2J")),
            format!("{name}�]52;c;secret���[2J")
        );
        assert_eq!(sgr(Color::Reset), None);
        assert_eq!(sgr(Color::LightRed).as_deref(), Some("91"));
        assert_eq!(sgr(Color::Gray).as_deref(), Some("37"));
    }
}
