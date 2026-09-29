//! The run's results as Go's view.go and servers.go write them (resultsView, finalReport and
//! detailsView), in styled lines the TUI draws and the report prints.
use crate::{
    model::{
        Ending, FailureScope, Phase, ServerContribution, ServerLatencyResult, ServerSummary, Snapshot, Stage,
        StageResult, StageStatus,
    },
    theme::Theme,
    vocabulary::{ADDED_NOTE, MISSING, clock, compact_population, compact_stage, population_label},
};
use graphite_meter_core::{failure::FailureReason, format, measurement::MeasurementResult, text::terminal_character};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::time::Duration;
use unicode_width::UnicodeWidthChar;

pub const WIDTH: usize = 100;
/// The arrows of a stage's directions, indexed as its results' down and up.
pub(crate) const ARROWS: [&str; 2] = ["↓", "↑"];

pub(crate) type Text = Vec<Line<'static>>;

/// Text in one style.
pub(crate) fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

/// A line in one style.
pub(crate) fn line(text: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(span(text, style))
}

pub(crate) fn cell(character: char) -> usize {
    character.width().unwrap_or(0)
}

/// Go's pad: spaces up to `to` cells.
pub(crate) fn pad(mut line: Line<'static>, to: usize) -> Line<'static> {
    line.spans.push(Span::raw(" ".repeat(to.saturating_sub(line.width()))));
    line
}

/// Go's ansi.Truncate with "…".
pub(crate) fn fit(line: Line<'static>, limit: usize) -> Line<'static> {
    truncate(line, limit.max(1), "…")
}

/// Go's ansi.Truncate: a wider line keeps what fits before the tail, in the style it cuts.
pub(crate) fn truncate(line: Line<'static>, limit: usize, tail: &str) -> Line<'static> {
    if line.width() <= limit {
        return line;
    }
    let mut room = limit.saturating_sub(tail.chars().map(cell).sum());
    let mut spans = Vec::new();
    for span in line.spans {
        let mut kept = String::new();
        for character in span.content.chars() {
            if cell(character) > room {
                spans.push(Span::styled(kept + tail, span.style));
                return Line::from(spans);
            }
            room -= cell(character);
            kept.push(character);
        }
        spans.push(Span::styled(kept, span.style));
    }
    Line::from(spans)
}

/// The line rendered in `base` as lipgloss nests styles: `base` underlies the first styled span,
/// whose reset leaves the rest of the line unstyled.
pub(crate) fn under(mut line: Line<'static>, base: Style) -> Line<'static> {
    let mut active = base;
    for span in &mut line.spans {
        if span.style == Style::default() {
            span.style = active;
        } else {
            span.style = active.patch(span.style);
            active = Style::default();
        }
    }
    line
}

/// The line's text without its styles.
pub(crate) fn plain(line: &Line) -> String {
    line.spans.iter().map(|span| span.content.as_ref()).collect()
}

/// The text with each character that could control the terminal replaced.
pub fn safe(text: &str) -> String {
    let shown = |character: char| Some(character).filter(|c| terminal_character(*c)).unwrap_or('�');
    text.chars().map(shown).collect()
}

/// Every span with its controls replaced, whatever sent the text; a plain theme also drops
/// every style, as colorprofile's NoTTY strips them.
pub(crate) fn sanitize(lines: &mut [Line<'static>], theme: &Theme) {
    let plain = *theme == Theme::default();
    for span in lines.iter_mut().flat_map(|line| line.spans.iter_mut()) {
        if !span.content.chars().all(terminal_character) {
            span.content = safe(&span.content).into();
        }
        if plain {
            span.style = Style::default();
        }
    }
}

/// Go's wrapParts: parts joined by " · " while a line holds `limit` characters.
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

/// The lines as a terminal prints them, each styled span in one SGR sequence as lipgloss writes
/// it: an ANSI colour (an index under 16) as 30–37 or 90–97, where crossterm would write 38;5.
pub(crate) fn ansi(lines: &[Line]) -> String {
    let color = |color, base: u8| match color {
        Some(Color::Indexed(index @ ..16)) => format!(";{}", base + index % 8 + index / 8 * 60),
        Some(Color::Indexed(index)) => format!(";{};5;{index}", base + 8),
        Some(Color::Rgb(red, green, blue)) => format!(";{};2;{red};{green};{blue}", base + 8),
        _ => String::new(),
    };
    let styled = |span: &Span| {
        let bold = span.style.add_modifier.contains(Modifier::BOLD);
        let bold = if bold { ";1" } else { "" };
        match format!("{bold}{}{}", color(span.style.fg, 30), color(span.style.bg, 40)).strip_prefix(';') {
            Some(codes) => format!("\x1b[{codes}m{}\x1b[m", span.content),
            None => span.content.to_string(),
        }
    };
    let lines = lines
        .iter()
        .map(|line| line.spans.iter().map(styled).collect::<String>());
    lines.collect::<Vec<_>>().join("\n")
}

/// Go's finalReport as lipgloss.Println prints it to stdout; none before a run reports.
pub fn print(snapshot: &Snapshot, width: usize) -> Option<String> {
    render(snapshot, width, Theme::terminal())
}

/// Asks the terminal for its background, as Go's runHeadless does before a report. Call it in raw mode.
pub async fn ask_background() {
    #[cfg(unix)]
    Theme::ask(Duration::from_secs(2)).await;
}

/// Go's finalReport in `theme`.
pub(crate) fn render(snapshot: &Snapshot, width: usize, theme: Theme) -> Option<String> {
    if snapshot.participants.is_empty() && snapshot.results.is_empty() {
        return None;
    }
    let report = Report::new(snapshot, snapshot.latency_focus.as_deref(), width, theme);
    let mut heading = "Latency".to_owned();
    if let Some(focus) = snapshot.latency_focus.as_deref().filter(|_| report.several()) {
        heading = format!("Latency to {}", server_name(snapshot, focus));
    }
    let results = report.results(&heading);
    let mut blocks = vec![vec![report.header()]];
    if !(results.throughput.is_empty() && results.latency.is_empty() && results.failures.is_empty()) {
        blocks.extend([report.throughput(), results.latency, results.failures]);
        blocks.push(report.notes(results.added));
    }
    if report.several() {
        blocks.push(report.details(false));
    }
    blocks.extend(snapshot.error.as_deref().map(|error| vec![line(error, theme.err)]));
    blocks.retain(|block| !block.is_empty());
    let mut lines: Text = blocks.join(&Line::default()).into_iter().map(trim_end).collect();
    sanitize(&mut lines, &theme);
    Some(ansi(&lines))
}

/// The line without trailing spaces.
fn trim_end(mut line: Line<'static>) -> Line<'static> {
    while let Some(last) = line.spans.pop() {
        let trimmed = last.content.trim_end_matches(' ');
        if !trimmed.is_empty() {
            line.spans.push(Span::styled(trimmed.to_owned(), last.style));
            break;
        }
    }
    line
}

/// The run's servers as Go's run details list them: every selected server the check reached.
pub(crate) fn run_servers(snapshot: &Snapshot) -> Vec<&ServerSummary> {
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

/// Go's statusLabel of a run.
pub fn status(snapshot: &Snapshot) -> &'static str {
    match snapshot.phase {
        Phase::Setup | Phase::Checking | Phase::Preparing => "Checking paths",
        Phase::Warmup => "Warmup",
        Phase::Measuring => snapshot.stage.map_or("Checking paths", Stage::name),
        phase => outcome(phase),
    }
}

/// Go's outcomeLabels.
pub fn outcome(phase: Phase) -> &'static str {
    match phase {
        Phase::Complete => "Complete",
        Phase::Partial => "Partial",
        Phase::Incomplete => "Incomplete",
        Phase::Cancelled => "Stopped",
        _ => "Failed",
    }
}

/// The directions a stage transfers, as indexes of `ARROWS`.
pub(crate) fn directions(stage: Stage) -> impl Iterator<Item = usize> {
    let moves = [stage.downloads(), stage.uploads()];
    (0..2).filter(move |direction| moves[*direction])
}

/// Go's directionLabel.
fn direction_label(stage: Stage, direction: usize) -> String {
    match stage {
        Stage::Bidirectional => format!("Bi-dir {}", ARROWS[direction]),
        stage => stage.name().to_owned(),
    }
}

/// Go's results: each grid, the failures under them, the notes Details shows and whether the
/// latency grid has an Added column.
#[derive(Default)]
pub(crate) struct Results {
    pub throughput: Text,
    pub latency: Text,
    pub failures: Text,
    pub notes: Text,
    pub added: bool,
}

impl Results {
    /// Go's results.view: the grids, then the failures.
    pub fn view(self) -> Text {
        [self.throughput, self.latency, self.failures].concat()
    }
}

pub(crate) struct Report<'a> {
    snapshot: &'a Snapshot,
    shown: Option<&'a str>,
    width: usize,
    theme: Theme,
}

impl<'a> Report<'a> {
    pub fn new(snapshot: &'a Snapshot, shown: Option<&'a str>, width: usize, theme: Theme) -> Self {
        Self {
            snapshot,
            shown,
            width,
            theme,
        }
    }

    /// Go's multipleRunServers.
    pub fn several(&self) -> bool {
        run_servers(self.snapshot).len() > 1
    }

    fn result(&self, stage: Stage) -> Option<&StageResult> {
        self.snapshot.results.iter().rfind(|result| result.stage == stage)
    }

    fn status(&self, stage: Stage) -> StageStatus {
        self.result(stage)
            .map_or(StageStatus::Skipped, |result| self.snapshot.stage_status(result))
    }

    /// Go's unmeasured: a stage's status, or "—" for one that ended well.
    fn unmeasured(&self, stage: Stage) -> &'static str {
        match self.status(stage) {
            StageStatus::Complete => MISSING,
            status => status.label(),
        }
    }

    fn measurement(&self, stage: Stage, direction: usize) -> Option<&MeasurementResult> {
        let result = self.result(stage)?;
        [&result.down, &result.up][direction].as_ref()
    }

    /// The shown server's latency in a stage.
    fn population(&self, stage: Stage) -> Option<&ServerLatencyResult> {
        let shown = self.shown?;
        self.result(stage)?
            .server_latencies
            .iter()
            .find(|host| host.id == shown)
    }

    /// Go's meanRates: each measured direction's arrow and mean rate.
    pub fn rates(&self, stage: Stage) -> String {
        let rates = directions(stage).filter_map(|direction| {
            let rate = self.measurement(stage, direction)?.mean_bytes_per_sec;
            Some(format!(
                "{} {}",
                ARROWS[direction],
                rate.map_or(MISSING.to_owned(), format::rate)
            ))
        });
        rates.collect::<Vec<_>>().join("  ")
    }

    /// Go's headline: a finished stage's rates, and an idle stage's median.
    pub fn headline(&self, stage: Stage) -> Vec<Span<'static>> {
        let rates = self.rates(stage);
        let mut spans: Vec<_> = (!rates.is_empty())
            .then(|| span(rates, self.theme.value))
            .into_iter()
            .collect();
        if let Some(median) = self.population(stage).and_then(ServerLatencyResult::median)
            && stage == Stage::Latency
        {
            spans.extend((!spans.is_empty()).then(|| Span::raw("  ")));
            spans.extend([span(ms(median), self.theme.value), span(" median", self.theme.muted)]);
        }
        spans
    }

    fn measured(&self) -> bool {
        self.snapshot
            .results
            .iter()
            .any(|result| result.down.is_some() || result.up.is_some())
            || self.snapshot.plan.iter().any(|stage| self.population(*stage).is_some())
    }

    /// Go's resultsView.
    pub fn results(&self, heading: &str) -> Results {
        let theme = &self.theme;
        let live = self.snapshot.phase.live();
        let idle = self.population(Stage::Latency).and_then(ServerLatencyResult::median);
        let mut out = Results::default();
        let (mut throughput, mut latency) = (Vec::new(), Vec::new());
        for stage in &self.snapshot.plan {
            let hue = theme.stage(*stage);
            if stage.downloads() || stage.uploads() {
                for direction in directions(*stage) {
                    let label = direction_label(*stage, direction);
                    if let Some(measurement) = self.measurement(*stage, direction)
                        && (measurement.mean_bytes_per_sec.is_some() || measurement.total_bytes > 0)
                    {
                        out.notes
                            .extend(self.note(&label, &throughput_facts(measurement, false)));
                    }
                    out.failures.extend(self.throughput_failure(*stage, direction, &label));
                }
                let mut rates = Line::from(self.rates(*stage));
                if rates.width() == 0 && !live {
                    rates = Line::from(self.unmeasured(*stage));
                } else if self.status(*stage) == StageStatus::Partial {
                    rates.spans.extend([Span::raw("  "), span("Partial", theme.warn)]);
                }
                if rates.width() > 0 {
                    throughput.push(vec![line(compact_stage(*stage), hue), rates]);
                }
            }
            let name = line(compact_population(*stage), hue);
            let Some(population) = self.population(*stage) else {
                if *stage == Stage::Latency && !live {
                    latency.push(vec![name, Line::from(self.unmeasured(*stage))]);
                }
                continue;
            };
            let mut cells = latency_cells(population, idle);
            if *stage == Stage::Latency {
                cells[1].clear();
            }
            out.added |= !cells[1].is_empty();
            latency.push([vec![name], cells.into_iter().map(Line::from).collect()].concat());
            let label = population_label(*stage);
            out.notes.extend(self.note(&label, &latency_facts(population)));
            if let Some(timing) = population.summary.reflector_timing {
                let label = format!("Server timing ({} paired replies, means)", count(timing.count));
                let (raw, handling) = (ms(timing.mean_raw_rtt), ms(timing.mean_handling));
                out.notes
                    .extend(self.note(&label, &[format!("raw {raw}"), format!("handling {handling}")]));
            }
            match population.ending {
                Some(Ending::Stopped) if self.snapshot.phase == Phase::Cancelled => {
                    out.failures.push(line(format!("{label} stopped."), theme.warn));
                }
                Some(Ending::Failed(reason)) => out
                    .failures
                    .push(line(format!("{label}: {}", reason.label()), theme.err)),
                _ => {}
            }
        }
        if !self.measured() {
            return Results::default();
        }
        if out.added {
            out.notes.push(line(ADDED_NOTE, theme.muted));
        }
        if !throughput.is_empty() {
            let scope = if self.several() { "All servers" } else { "" };
            out.throughput = self.grid(&["Throughput", scope], throughput);
        }
        if !latency.is_empty() {
            let mut headers = vec![heading, "Median", "Added", "P95", "Jitter", "Probe timeouts"];
            for row in &mut latency {
                row.resize(headers.len(), Line::default());
                if !out.added {
                    row.remove(2);
                }
            }
            if !out.added {
                headers.remove(2);
            }
            out.latency = self.grid(&headers, latency);
        }
        out
    }

    /// Why a direction has no result, or ended early.
    fn throughput_failure(&self, stage: Stage, direction: usize, label: &str) -> Option<Line<'static>> {
        let result = self.result(stage)?;
        let measurement = self.measurement(stage, direction)?;
        if result.stopped {
            return Some(line(format!("{label} stopped."), self.theme.warn));
        }
        let failures = self.snapshot.failures.iter();
        let failures: Vec<_> = failures
            .filter(|failure| failure.stage == stage && failure.scope == FailureScope::Throughput)
            .collect();
        let left = |server: &ServerContribution| failures.iter().any(|failure| failure.server_id == server.id);
        let all_left = !result.server_results.is_empty() && result.server_results.iter().all(left);
        let reason = match failures.last() {
            Some(failure) if all_left => failure.reason,
            _ if measurement.mean_bytes_per_sec.is_none() => FailureReason::InsufficientEvidence,
            _ => return None,
        };
        Some(line(format!("{label}: {}", reason.label()), self.theme.err))
    }

    /// Go's reportHeader.
    pub fn header(&self) -> Line<'static> {
        let mut facts = match run_servers(self.snapshot).as_slice() {
            [] => Vec::new(),
            [server] => vec![server.name.clone()],
            servers => vec![format!("{} servers", servers.len())],
        };
        facts.push(clock(self.snapshot.duration));
        let results = self.snapshot.results.iter();
        let total: u64 = results.map(|result| result.down_bytes() + result.up_bytes()).sum();
        if total > 0 {
            facts.push(format::bytes(total));
        }
        let phase = self.snapshot.phase;
        let mut tone = Style::new().add_modifier(Modifier::BOLD);
        tone.fg = self.theme.outcome(phase).bg;
        let title = span("Graphite Meter", self.theme.heading);
        let facts = span(facts.join(" · "), self.theme.muted);
        Line::from(vec![
            title,
            Span::raw("  "),
            span(outcome(phase), tone),
            Span::raw("  "),
            facts,
        ])
    }

    /// Go's throughputReport: each planned direction's rate and facts.
    pub fn throughput(&self) -> Text {
        let theme = &self.theme;
        let mut rows = Vec::new();
        for stage in &self.snapshot.plan {
            let hue = theme.stage(*stage);
            for direction in directions(*stage) {
                let label = Line::from(vec![
                    span(ARROWS[direction], hue),
                    Span::raw(" "),
                    span(stage.name(), theme.text),
                ]);
                let (value, mut facts) = match self.measurement(*stage, direction) {
                    None => (line(self.unmeasured(*stage), theme.muted), Vec::new()),
                    Some(measurement) => match measurement.mean_bytes_per_sec {
                        None => (line(MISSING, theme.muted), Vec::new()),
                        Some(mean) => {
                            let bold = hue.add_modifier(Modifier::BOLD);
                            (line(format::rate(mean), bold), throughput_facts(measurement, true))
                        }
                    },
                };
                let partial = self.status(*stage) == StageStatus::Partial;
                if partial {
                    facts.insert(0, "Partial".to_owned());
                }
                rows.push((label, value, facts, partial));
            }
        }
        let label_width = rows.iter().map(|row| row.0.width()).max().unwrap_or(0);
        let value_width = rows.iter().map(|row| row.1.width()).max().unwrap_or(0);
        let indent = Line::from(" ".repeat(label_width + value_width + 2));
        let mut lines = Vec::new();
        for (label, value, facts, partial) in rows {
            let mut first = pad(label, label_width);
            first.spans.push(Span::raw("  "));
            first.spans.extend(pad(value, value_width).spans);
            let parts = wrap_parts(&facts, self.width.saturating_sub(indent.width() + 3).max(20));
            for (index, part) in parts.into_iter().enumerate() {
                let mut line = if index == 0 { first.clone() } else { indent.clone() };
                if index == 0 && partial {
                    let rest = part.strip_prefix("Partial").unwrap_or(&part).to_owned();
                    line.spans
                        .extend([Span::raw("   "), span("Partial", theme.warn), span(rest, theme.muted)]);
                } else if !part.is_empty() {
                    line.spans.extend([Span::raw("   "), span(part, theme.muted)]);
                }
                lines.push(line);
            }
        }
        lines
    }

    /// Go's reportNotes: latency issues, the server's share of the round trips, and Added.
    pub fn notes(&self, added: bool) -> Text {
        let (mut notes, mut timing) = (Vec::new(), Vec::new());
        for stage in &self.snapshot.plan {
            let Some(population) = self.population(*stage) else {
                continue;
            };
            let summary = population.summary;
            let issues = summary.timeouts > 0 || summary.unresolved > 0 || summary.send_failures > 0;
            if population.ending.is_some() || issues {
                notes.extend(self.note(&population_label(*stage), &latency_facts(population)));
            }
            if let Some(reflector) = summary.reflector_timing {
                let (handling, raw) = (ms(reflector.mean_handling), ms(reflector.mean_raw_rtt));
                let mut part = format!("{} {handling} of {raw}", compact_population(*stage));
                if reflector.count != summary.count {
                    part.push_str(&format!(" ({} pairs)", count(reflector.count)));
                }
                timing.push(part);
            }
        }
        if !timing.is_empty() {
            notes.extend(self.note("Server handling of the mean round trip", &timing));
        }
        if added {
            notes.push(line(ADDED_NOTE, self.theme.muted));
        }
        notes
    }

    /// Go's note: the label and its facts, wrapped under the label when they do not fit beside it.
    fn note(&self, label: &str, facts: &[String]) -> Text {
        let first = format!("{label}: {}", facts[0]);
        let (mut lines, facts) = if first.chars().map(cell).sum::<usize>() <= self.width.saturating_sub(2) {
            (Vec::new(), [vec![first], facts[1..].to_vec()].concat())
        } else {
            (vec![format!("{label}:")], facts.to_vec())
        };
        for line in wrap_parts(&facts, self.width.saturating_sub(2)) {
            lines.push(if lines.is_empty() { line } else { format!("  {line}") });
        }
        lines.into_iter().map(|text| line(text, self.theme.muted)).collect()
    }

    /// Go's detailsView: the outcome, the facts in full, the mean rates and latency medians of
    /// the run and each server (✗ for one that left), the issues and, in full once the run ends,
    /// its aggregation intervals.
    pub fn details(&self, full: bool) -> Text {
        let (theme, run) = (&self.theme, self.snapshot);
        let mut columns = Vec::new();
        for stage in &run.plan {
            columns.extend(directions(*stage).map(|at| (*stage, at)));
        }
        let mean = |measurement: Option<&MeasurementResult>| {
            let mean = measurement.and_then(|measurement| measurement.mean_bytes_per_sec);
            Line::from(mean.map_or_else(|| MISSING.to_owned(), format::rate))
        };
        let mut rates = vec![vec![Line::from("All servers")]];
        rates[0].extend(columns.iter().map(|(stage, at)| mean(self.measurement(*stage, *at))));
        let mut medians = Vec::new();
        for server in run_servers(run) {
            let own = |stage| {
                self.result(stage)?
                    .server_results
                    .iter()
                    .find(|own| own.id == server.id)
            };
            let own =
                |(stage, at): &(Stage, usize)| mean(own(*stage).and_then(|own| [&own.down, &own.up][*at].as_ref()));
            let remains = run.participants.contains(&server.id);
            let name = Line::from(format!("{}{}", server.name, if remains { "" } else { " ✗" }));
            rates.push(std::iter::once(name).chain(columns.iter().map(own)).collect());
            let median = |stage: &Stage| {
                let hosts = self.result(*stage).map_or(&[][..], |result| &result.server_latencies);
                let median = hosts
                    .iter()
                    .find(|host| host.id == server.id)
                    .and_then(ServerLatencyResult::median);
                Line::from(median.map_or(MISSING.into(), ms))
            };
            let name = Line::from(server.name.clone());
            medians.push(std::iter::once(name).chain(run.plan.iter().map(median)).collect());
        }
        let mut headers = vec!["Server".to_owned()];
        headers.extend(columns.iter().map(|(stage, at)| direction_label(*stage, *at)));
        let mut populations = vec!["Server"];
        populations.extend(run.plan.iter().map(|stage| compact_population(*stage)));
        let mut lines = vec![line(self.outcome_notice(), theme.heading)];
        let notes = self.results("Latency").notes;
        if full && !notes.is_empty() {
            lines.extend(notes);
            lines.push(Line::default());
        }
        lines.extend(self.grid(&headers, rates));
        lines.extend([Line::default(), line("Latency median by server", theme.heading)]);
        lines.extend(self.grid(&populations, medians));
        if !run.failures.is_empty() {
            lines.extend([Line::default(), line("Issues", theme.heading)]);
        }
        for failure in &run.failures {
            let throughput = failure.scope == FailureScope::Throughput;
            let scope = if throughput { "throughput" } else { "latency" };
            let (name, stage) = (server_name(run, &failure.server_id), compact_stage(failure.stage));
            let (at, reason) = (clock(failure.at), failure.reason.label());
            lines.push(Line::from(format!("{name} · {stage} {scope} · at {at} · {reason}")));
        }
        if full && !run.phase.live() && !run.intervals.is_empty() {
            lines.extend([Line::default(), line("Aggregation intervals", theme.heading)]);
            for interval in &run.intervals {
                let seconds = |nanos| Duration::from_nanos(nanos).as_secs_f64();
                let (start, end) = (seconds(interval.start_nanos), seconds(interval.end_nanos));
                let names: Vec<_> = interval.participants.iter().map(|id| server_name(run, id)).collect();
                let state = match interval.complete && interval.window.is_some() {
                    true => "measured window",
                    false => "incomplete evidence",
                };
                let stage = compact_stage(interval.stage.into());
                let mut parts = vec![format!("{stage} {start:.1}–{end:.1} s"), names.join(", "), state.into()];
                parts.retain(|part| !part.is_empty());
                lines.push(line(parts.join(" · "), theme.muted));
            }
            if run.omitted_intervals > 0 {
                let omitted = run.omitted_intervals;
                let text = format!("{omitted} older intervals omitted; byte totals retain the full run");
                lines.push(line(text, theme.muted));
            }
        }
        lines.into_iter().map(|line| fit(line, self.width)).collect()
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

    /// Go's grid: muted headers over text cells, or each row's facts when the columns do not fit.
    fn grid(&self, headers: &[impl AsRef<str>], rows: Vec<Vec<Line<'static>>>) -> Text {
        let (muted, text) = (self.theme.muted, self.theme.text);
        let headers: Vec<_> = headers
            .iter()
            .map(|header| Line::from(header.as_ref().to_owned()))
            .collect();
        let mut widths = vec![0; headers.len()];
        for row in std::iter::once(&headers).chain(&rows) {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell.width());
            }
        }
        if widths.iter().map(|width| width + 2).sum::<usize>().saturating_sub(2) <= self.width {
            let styled = std::iter::once((headers, muted)).chain(rows.into_iter().map(|row| (row, text)));
            let row = |(cells, base): (Vec<Line<'static>>, Style)| {
                let cells = cells.into_iter().zip(&widths);
                let cells: Vec<_> = cells
                    .map(|(cell, width)| pad(under(cell, base), *width).spans)
                    .collect();
                trim_end(Line::from(cells.join(&Span::raw("  "))))
            };
            return styled.map(row).collect();
        }
        let mut lines = vec![under(headers[0].clone(), muted)];
        for row in rows {
            let facts = row[1..].iter().zip(&headers[1..]).filter(|(cell, _)| cell.width() > 0);
            let facts: Vec<_> = facts
                .map(|(cell, header)| format!("{} {}", plain(header), plain(cell)))
                .collect();
            let facts: Vec<_> = facts.iter().map(|fact| fact.trim().to_owned()).collect();
            lines.push(under(row[0].clone(), text));
            let facts = wrap_parts(&facts, self.width.saturating_sub(2));
            lines.extend(
                facts
                    .into_iter()
                    .map(|fact| Line::from(vec![Span::raw("  "), span(fact, muted)])),
            );
        }
        lines
    }
}

/// Go's latencyCells: median, Added, P95, jitter and probe timeouts.
fn latency_cells(population: &ServerLatencyResult, idle: Option<u64>) -> Vec<String> {
    let (summary, median) = (population.summary, population.median());
    let added = median
        .zip(idle)
        .map(|(median, idle)| (median as f64 - idle as f64) / 1e6);
    let added = added.map(|added| format!("{} ms", format::added_ms(added)));
    let p95 = summary.distribution.map(|distribution| ms(distribution.p95));
    let cells = [median.map(ms), added, p95, summary.jitter.map(ms)];
    let mut cells: Vec<_> = cells
        .into_iter()
        .map(|cell| cell.unwrap_or_else(|| MISSING.to_owned()))
        .collect();
    cells.push(MISSING.to_owned());
    if let Some(ratio) = summary.timeout_ratio() {
        cells[4] = format!(
            "{} / {}",
            count(summary.timeouts),
            count(summary.count + summary.timeouts)
        );
        if ratio > 0.0 {
            let precision = if ratio >= 0.01 { 1 } else { 2 };
            cells[4].push_str(&format!(" ({:.precision$}%)", ratio * 100.0));
        }
    }
    cells
}

/// Go's latencyFacts.
fn latency_facts(population: &ServerLatencyResult) -> Vec<String> {
    let summary = population.summary;
    let mut facts = vec![format!("{} replies", count(summary.count))];
    facts.extend(population.elapsed.filter(|elapsed| !elapsed.is_zero()).map(clock));
    if summary.unresolved > 0 {
        facts.push(format!("unfinished probes {}", count(summary.unresolved)));
    }
    if summary.send_failures > 0 {
        facts.push(format!("failed sends {}", count(summary.send_failures)));
    }
    facts
}

/// Go's throughputFacts; `brief` leaves out the samples and the peak's unit when the mean has it.
fn throughput_facts(measurement: &MeasurementResult, brief: bool) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(peak) = measurement.peak_bytes_per_sec.filter(|peak| *peak > 0.0) {
        let peak = format::rate(peak);
        let mean = measurement.mean_bytes_per_sec.filter(|_| brief).map(format::rate);
        let unit = mean.and_then(|mean| Some(format!(" {}", mean.rsplit_once(' ')?.1)));
        let peak = unit.and_then(|unit| peak.strip_suffix(&unit)).unwrap_or(&peak);
        facts.push(format!("peak {peak}"));
    }
    facts.push(format::bytes(measurement.total_bytes));
    let elapsed = measurement.elapsed_nanos.filter(|elapsed| *elapsed > 0);
    facts.extend(elapsed.map(|elapsed| clock(Duration::from_nanos(elapsed))));
    if measurement.samples > 0 && !brief {
        facts.push(format!("{} samples", count(measurement.samples)));
    }
    if measurement.direction == graphite_meter_core::measurement::Direction::Up {
        facts.push("receiver-timed".to_owned());
    }
    facts
}

/// Go's fmtMs of nanoseconds.
pub(crate) fn ms(nanos: u64) -> String {
    format!("{} ms", format::latency_ms(nanos as f64 / 1e6))
}

/// Go's fmtCount: thousands separated by commas.
fn count(value: usize) -> String {
    let digits = value.to_string();
    let groups = digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|group| String::from_utf8_lossy(group));
    groups.collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::ServerFailure,
        theme::Profile,
        ui::tests::{latency_result, lost, probes, result},
    };

    /// A server of the run, as the catalogue names it.
    fn server(id: &str, name: &str) -> ServerSummary {
        let (id, name, error) = (id.into(), name.into(), Some("checked".into()));
        ServerSummary {
            id,
            name,
            error,
            ..ServerSummary::default()
        }
    }

    /// One server's idle and loaded latency and a download a late probe timeout made partial.
    fn partial_run() -> Snapshot {
        let (mut idle, mut download) = (
            result(Stage::Latency, None, None),
            result(Stage::Download, Some(1.5e6), None),
        );
        idle.server_latencies = vec![latency_result("a", probes(&[1_000_000, 2_000_000, 3_000_000], 1))];
        download.down.as_mut().unwrap().peak_bytes_per_sec = Some(1_800_000.0);
        download.server_latencies = vec![latency_result("a", probes(&[5_000_000, 6_000_000, 7_000_000], 0))];
        download.server_results = vec![ServerContribution::default()];
        download.server_results[0].id = "a".into();
        let (scope, reason, at) = (FailureScope::Latency, FailureReason::Timeout, Duration::from_secs(3));
        Snapshot {
            phase: Phase::Partial,
            servers: vec![server("a", "Alpha")],
            participants: vec!["a".into()],
            latency_focus: Some("a".into()),
            plan: vec![Stage::Latency, Stage::Download],
            duration: Duration::from_secs(5),
            results: vec![idle, download],
            failures: vec![ServerFailure {
                scope,
                reason,
                at,
                ..lost("a", Stage::Download)
            }],
            error: Some("Stopped delivering data".into()),
            ..Snapshot::default()
        }
    }

    /// The report as Go prints it, then as a terminal paints it; several servers add their details.
    #[test]
    fn the_report_reads_and_paints_as_go_prints_it() {
        let report = render(&partial_run(), WIDTH, Theme::default()).unwrap();
        assert_eq!(
            report,
            [
                "Graphite Meter  Partial  Alpha · 5.0 s · 1.5 MB",
                "",
                "↓ Download  12.00 Mbit/s   Partial · peak 14.40 · 1.5 MB · 1.0 s",
                "",
                "Latency      Median  Added    P95     Jitter  Probe timeouts",
                "Idle         2.0 ms           3.0 ms  1.0 ms  1 / 4 (25.0%)",
                "Loaded down  6.0 ms  +4.0 ms  7.0 ms  1.0 ms  0 / 3",
                "",
                "Idle latency: 3 replies · 1.0 s",
                "Server handling of the mean round trip: Idle < 0.1 ms of 2.0 ms · Loaded down < 0.1 ms of 6.0 ms",
                "Added: loaded median minus idle median, same server.",
                "",
                "Stopped delivering data",
            ]
            .join("\n")
        );
        let mut pair = partial_run();
        // Painted as lipgloss writes each profile: 16 colours as 30–37 and 90–97.
        #[rustfmt::skip]
        let paints = [(Profile::Ansi256, ["1;38;5;254mGraphite Meter", "1;38;5;186mPartial", "1;38;5;75m12.00 Mbit/s"]),
            (Profile::Ansi, ["\x1b[1;97mGraphite Meter\x1b[m  \x1b[1;93mPartial\x1b[m", "1;94m12.00", "1;91mStopped"])];
        for (profile, parts) in paints {
            let painted = render(&pair, WIDTH, Theme::new(profile, true)).unwrap();
            assert!(parts.iter().all(|part| painted.contains(part)), "{painted:?}");
        }
        pair.servers.push(server("b", "Beta"));
        let text = render(&pair, WIDTH, Theme::default()).unwrap();
        for line in [
            "Partial · 1 of 2 servers",
            "Latency median by server",
            "Issues",
            "Alpha · Download latency · at 3.0 s · Stopped delivering data",
        ] {
            assert!(text.lines().any(|shown| shown == line), "{line:?} in {text}");
        }
    }

    #[test]
    fn lines_fit_pad_and_sanitize_around_their_styles() {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let styled = Line::from(vec![span("ab", bold), Span::raw("cdef")]);
        assert_eq!(
            fit(styled.clone(), 4),
            Line::from(vec![span("ab", bold), Span::raw("c…")])
        );
        assert_eq!(fit(styled.clone(), 6), styled);
        assert_eq!(pad(Line::from("ab"), 4).width(), 4);
        let mut lines = vec![Line::from(span("name\x1b]52;c;secret\x07\u{202e}", bold))];
        sanitize(&mut lines, &Theme::default());
        assert_eq!(lines[0].spans[0].style, Style::default());
        assert_eq!(plain(&lines[0]), "name�]52;c;secret��");
        assert_eq!(ansi(&[Line::from(span("x", bold))]), "\x1b[1mx\x1b[m");
    }
}
