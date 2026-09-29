//! The run's results as Go's view.go and servers.go write them (resultsView, finalReport and
//! detailsView), in styled lines the TUI draws and the report prints.
use crate::{
    model::{
        Ending, FailureScope, Phase, ServerFailure, ServerLatencyResult, ServerSummary, Snapshot, Stage, StageResult,
        StageStatus,
    },
    theme::Theme,
    vocabulary::{ADDED_NOTE, MISSING, clock, compact_population, compact_stage, population_label},
};
use crossterm::style::ContentStyle;
use graphite_meter_core::{failure::FailureReason, format, measurement::MeasurementResult, text::terminal_character};
use ratatui::{
    backend::IntoCrossterm,
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

pub const WIDTH: usize = 100;

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
    let fill = to.saturating_sub(line.width());
    if fill > 0 {
        line.spans.push(Span::raw(" ".repeat(fill)));
    }
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
                kept.push_str(tail);
                spans.push(Span::styled(kept, span.style));
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

/// A character the terminal shows as itself, or the replacement for one that could control it.
pub fn terminal_char(character: char) -> char {
    if terminal_character(character) {
        character
    } else {
        '�'
    }
}

/// Every span with its controls replaced, whatever sent the text; a plain theme also drops
/// every style, as colorprofile's NoTTY strips them.
pub(crate) fn sanitize(lines: &mut [Line<'static>], theme: &Theme) {
    let plain = *theme == Theme::default();
    for span in lines.iter_mut().flat_map(|line| line.spans.iter_mut()) {
        if !span.content.chars().all(terminal_character) {
            span.content = span.content.chars().map(terminal_char).collect::<String>().into();
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

/// The lines as a terminal prints them: each span in its style, as crossterm draws the TUI.
pub(crate) fn ansi(lines: &[Line]) -> String {
    let styled = |span: &Span| {
        let style: ContentStyle = span.style.into_crossterm();
        style.apply(span.content.as_ref()).to_string()
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
    Theme::ask(std::time::Duration::from_secs(2)).await;
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
    if !results.view().is_empty() {
        blocks.extend([
            report.throughput(),
            results.latency,
            results.failures,
            report.notes(results.added),
        ]);
    }
    if report.several() {
        blocks.push(report.details(false));
    }
    blocks.extend(snapshot.error.as_deref().map(|error| vec![line(error, theme.err)]));
    let mut lines = Vec::new();
    for block in blocks.into_iter().filter(|block| !block.is_empty()) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(block.into_iter().map(trim_end));
    }
    sanitize(&mut lines, &theme);
    Some(ansi(&lines))
}

/// Go's detailsView in full, as plain text.
pub fn details(snapshot: &Snapshot, shown: Option<&str>, width: usize) -> String {
    let lines = Report::new(snapshot, shown, width, Theme::default()).details(true);
    lines.iter().map(plain).collect::<Vec<_>>().join("\n")
}

/// The line without trailing spaces.
fn trim_end(mut line: Line<'static>) -> Line<'static> {
    while let Some(last) = line.spans.last_mut() {
        let trimmed = last.content.trim_end_matches(' ');
        if !trimmed.is_empty() {
            let trimmed = trimmed.to_owned();
            last.content = trimmed.into();
            break;
        }
        line.spans.pop();
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
    pub fn view(&self) -> Text {
        [&self.throughput, &self.latency, &self.failures]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

pub(crate) struct Report<'a> {
    snapshot: &'a Snapshot,
    shown: Option<&'a str>,
    width: usize,
    theme: Theme,
}

#[derive(Clone, Copy)]
enum Direction {
    Down,
    Up,
}

impl Direction {
    /// The directions a stage transfers.
    fn of(stage: Stage) -> impl Iterator<Item = Self> {
        [
            stage.downloads().then_some(Self::Down),
            stage.uploads().then_some(Self::Up),
        ]
        .into_iter()
        .flatten()
    }

    fn arrow(self) -> &'static str {
        match self {
            Self::Down => "↓",
            Self::Up => "↑",
        }
    }

    /// Go's directionLabel.
    fn label(self, stage: Stage) -> String {
        match stage {
            Stage::Bidirectional => format!("Bi-dir {}", self.arrow()),
            stage => stage.name().to_owned(),
        }
    }
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

    fn measurement(&self, stage: Stage, direction: Direction) -> Option<&MeasurementResult> {
        let result = self.result(stage)?;
        match direction {
            Direction::Down => result.down.as_ref(),
            Direction::Up => result.up.as_ref(),
        }
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
        let rates = Direction::of(stage).filter_map(|direction| {
            let rate = self.measurement(stage, direction)?.mean_bytes_per_sec;
            Some(format!(
                "{} {}",
                direction.arrow(),
                rate.map_or(MISSING.to_owned(), format::rate)
            ))
        });
        rates.collect::<Vec<_>>().join("  ")
    }

    /// Go's headline: a finished stage's rates, and an idle stage's median.
    pub fn headline(&self, stage: Stage) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let rates = self.rates(stage);
        if !rates.is_empty() {
            spans.push(span(rates, self.theme.value));
        }
        if let Some(median) = self.population(stage).and_then(ServerLatencyResult::median)
            && stage == Stage::Latency
        {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
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
                for direction in Direction::of(*stage) {
                    let label = direction.label(*stage);
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
            match self.population(*stage) {
                Some(population) => {
                    let mut cells = latency_cells(population, idle);
                    if *stage == Stage::Latency {
                        cells[1].clear();
                    }
                    out.added |= !cells[1].is_empty();
                    let mut row = vec![line(compact_population(*stage), hue)];
                    row.extend(cells.into_iter().map(Line::from));
                    latency.push(row);
                    let label = population_label(*stage);
                    out.notes.extend(self.note(&label, &latency_facts(population)));
                    if let Some(timing) = population.summary.reflector_timing {
                        let label = format!("Server timing ({} paired replies, means)", count(timing.count));
                        let facts = [
                            format!("raw {}", ms(timing.mean_raw_rtt)),
                            format!("handling {}", ms(timing.mean_handling)),
                        ];
                        out.notes.extend(self.note(&label, &facts));
                    }
                    match population.ending {
                        Some(Ending::Stopped) if self.snapshot.phase == Phase::Cancelled => {
                            out.failures.push(line(format!("{label} stopped."), theme.warn));
                        }
                        Some(Ending::Failed(reason)) => {
                            out.failures
                                .push(line(format!("{label}: {}", reason.label()), theme.err));
                        }
                        _ => {}
                    }
                }
                None if *stage == Stage::Latency && !live => {
                    latency.push(vec![
                        line(compact_population(*stage), hue),
                        Line::from(self.unmeasured(*stage)),
                    ]);
                }
                None => {}
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
    fn throughput_failure(&self, stage: Stage, direction: Direction, label: &str) -> Option<Line<'static>> {
        let result = self.result(stage)?;
        let measurement = self.measurement(stage, direction)?;
        if result.stopped {
            return Some(line(format!("{label} stopped."), self.theme.warn));
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
        Some(line(format!("{label}: {}", reason.label()), self.theme.err))
    }

    /// Go's reportHeader.
    pub fn header(&self) -> Line<'static> {
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
            .flat_map(|result| [&result.down, &result.up])
            .flatten()
            .map(|measurement| measurement.total_bytes)
            .sum();
        if total > 0 {
            facts.push(format::bytes(total));
        }
        let phase = self.snapshot.phase;
        let tone = Style {
            fg: self.theme.outcome(phase).bg,
            ..Style::new().add_modifier(Modifier::BOLD)
        };
        Line::from(vec![
            span("Graphite Meter", self.theme.heading),
            Span::raw("  "),
            span(outcome(phase), tone),
            Span::raw("  "),
            span(facts.join(" · "), self.theme.muted),
        ])
    }

    /// Go's throughputReport: each planned direction's rate and facts.
    pub fn throughput(&self) -> Text {
        let theme = &self.theme;
        let mut rows = Vec::new();
        for stage in &self.snapshot.plan {
            let hue = theme.stage(*stage);
            for direction in Direction::of(*stage) {
                let label = Line::from(vec![
                    span(direction.arrow(), hue),
                    Span::raw(" "),
                    span(stage.name(), theme.text),
                ]);
                let (value, mut facts) = match self.measurement(*stage, direction) {
                    None => (line(self.unmeasured(*stage), theme.muted), Vec::new()),
                    Some(measurement) => match measurement.mean_bytes_per_sec {
                        None => (line(MISSING, theme.muted), Vec::new()),
                        Some(mean) => (
                            line(format::rate(mean), hue.add_modifier(Modifier::BOLD)),
                            throughput_facts(measurement, true),
                        ),
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
        let indent = label_width + value_width + 2;
        let mut lines = Vec::new();
        for (label, value, facts, partial) in rows {
            let mut first = pad(label, label_width);
            first.spans.push(Span::raw("  "));
            first.spans.extend(pad(value, value_width).spans);
            for (index, part) in wrap_parts(&facts, self.width.saturating_sub(indent + 3).max(20))
                .into_iter()
                .enumerate()
            {
                let mut line = if index == 0 {
                    first.clone()
                } else {
                    Line::from(" ".repeat(indent))
                };
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
        let mut notes = Vec::new();
        let mut timing = Vec::new();
        for stage in &self.snapshot.plan {
            let Some(population) = self.population(*stage) else {
                continue;
            };
            let summary = population.summary;
            if population.ending.is_some()
                || summary.timeouts > 0
                || summary.unresolved > 0
                || summary.send_failures > 0
            {
                notes.extend(self.note(&population_label(*stage), &latency_facts(population)));
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

    /// Go's detailsView: the outcome, the facts in full, the rates and latency medians by server,
    /// the issues and, in full once the run ends, its aggregation intervals.
    pub fn details(&self, full: bool) -> Text {
        let theme = &self.theme;
        let mut lines = vec![line(self.outcome_notice(), theme.heading)];
        let notes = self.results("Latency").notes;
        if full && !notes.is_empty() {
            lines.extend(notes);
            lines.push(Line::default());
        }
        lines.extend(self.server_rates());
        lines.extend([Line::default(), line("Latency median by server", theme.heading)]);
        lines.extend(self.server_medians());
        if !self.snapshot.failures.is_empty() {
            lines.extend([Line::default(), line("Issues", theme.heading)]);
            lines.extend(
                self.snapshot
                    .failures
                    .iter()
                    .map(|failure| Line::from(self.issue(failure))),
            );
        }
        if full && !self.snapshot.phase.live() && !self.snapshot.intervals.is_empty() {
            lines.extend([Line::default(), line("Aggregation intervals", theme.heading)]);
            lines.extend(self.intervals().into_iter().map(|text| line(text, theme.muted)));
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

    fn directions(&self) -> Vec<(Stage, Direction)> {
        let plan = self.snapshot.plan.iter();
        plan.flat_map(|stage| Direction::of(*stage).map(|direction| (*stage, direction)))
            .collect()
    }

    /// Each direction's mean rate for the run, then for each server; one that left is marked ✗.
    fn server_rates(&self) -> Text {
        let directions = self.directions();
        let rate = |measurement: Option<&MeasurementResult>| {
            let mean = measurement.and_then(|measurement| measurement.mean_bytes_per_sec);
            Line::from(mean.map_or_else(|| MISSING.to_owned(), format::rate))
        };
        let mut headers = vec!["Server".to_owned()];
        headers.extend(directions.iter().map(|(stage, direction)| direction.label(*stage)));
        let mut all = vec![Line::from("All servers")];
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
            let mut name = server.name.clone();
            if !self.snapshot.participants.contains(&server.id) {
                name.push_str(" ✗");
            }
            let mut row = vec![Line::from(name)];
            row.extend(
                directions
                    .iter()
                    .map(|(stage, direction)| rate(own(*stage, *direction))),
            );
            rows.push(row);
        }
        self.grid(&headers.iter().map(String::as_str).collect::<Vec<_>>(), rows)
    }

    /// Each server's latency median in each planned stage.
    fn server_medians(&self) -> Text {
        let mut headers = vec!["Server"];
        headers.extend(self.snapshot.plan.iter().map(|stage| compact_population(*stage)));
        let rows: Vec<_> = run_servers(self.snapshot)
            .into_iter()
            .map(|server| {
                let median = |stage: &Stage| {
                    let text = self
                        .result(*stage)
                        .and_then(|result| result.server_latencies.iter().find(|host| host.id == server.id))
                        .and_then(ServerLatencyResult::median)
                        .map_or_else(|| MISSING.to_owned(), ms);
                    Line::from(text)
                };
                std::iter::once(Line::from(server.name.clone()))
                    .chain(self.snapshot.plan.iter().map(median))
                    .collect()
            })
            .collect();
        self.grid(&headers, rows)
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
        let seconds = |nanos| std::time::Duration::from_nanos(nanos).as_secs_f64();
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

    /// Go's grid: muted headers over text cells, or each row's facts when the columns do not fit.
    fn grid(&self, headers: &[&str], rows: Vec<Vec<Line<'static>>>) -> Text {
        let (muted, text) = (self.theme.muted, self.theme.text);
        let headers: Vec<_> = headers.iter().map(|header| Line::from(header.to_string())).collect();
        let mut widths = vec![0; headers.len()];
        for row in std::iter::once(&headers).chain(&rows) {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell.width());
            }
        }
        if widths.iter().map(|width| width + 2).sum::<usize>().saturating_sub(2) <= self.width {
            let styled = std::iter::once((headers, muted)).chain(rows.into_iter().map(|row| (row, text)));
            return styled
                .map(|(cells, base)| {
                    let mut spans = Vec::new();
                    for (index, (cell, width)) in cells.into_iter().zip(&widths).enumerate() {
                        if index > 0 {
                            spans.push(Span::raw("  "));
                        }
                        spans.extend(pad(under(cell, base), *width).spans);
                    }
                    trim_end(Line::from(spans))
                })
                .collect();
        }
        let mut lines = vec![under(headers[0].clone(), muted)];
        for row in rows {
            let facts: Vec<_> = row[1..]
                .iter()
                .zip(&headers[1..])
                .filter(|(cell, _)| cell.width() > 0)
                .map(|(cell, header)| format!("{} {}", plain(header), plain(cell)).trim().to_owned())
                .collect();
            lines.push(under(row[0].clone(), text));
            lines.extend(
                wrap_parts(&facts, self.width.saturating_sub(2))
                    .into_iter()
                    .map(|fact| Line::from(vec![Span::raw("  "), span(fact, muted)])),
            );
        }
        lines
    }
}

/// Go's latencyCells: median, Added, P95, jitter and probe timeouts.
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

/// Go's latencyFacts.
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

/// Go's throughputFacts; `brief` leaves out the samples and the peak's unit when the mean has it.
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
        facts.push(clock(std::time::Duration::from_nanos(elapsed)));
    }
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

/// Go's fmtCount.
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
    use crate::{
        model::ServerContribution,
        theme::Profile,
        ui::tests::{download_measurement, latency_result, probes},
    };

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
            duration: std::time::Duration::from_secs(5),
            results: vec![
                StageResult {
                    stage: Stage::Latency,
                    elapsed: std::time::Duration::from_secs(1),
                    server_latencies: latency(&[1_000_000, 2_000_000, 3_000_000], 1),
                    ..StageResult::default()
                },
                StageResult {
                    stage: Stage::Download,
                    elapsed: std::time::Duration::from_secs(1),
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
                at: std::time::Duration::from_secs(3),
            }],
            error: Some("Stopped delivering data".into()),
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_report_reads_as_go_prints_it() {
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
    }

    #[test]
    fn a_terminal_report_paints_go_styles_over_the_plain_text() {
        let snapshot = partial_run();
        let theme = Theme::new(Profile::Ansi256, true);
        let painted = render(&snapshot, WIDTH, theme).unwrap();
        assert!(painted.starts_with("\x1b[38;5;254m\x1b[1mGraphite Meter\x1b[0m  \x1b[38;5;186m\x1b[1mPartial\x1b[0m"));
        assert!(
            painted.contains("\x1b[38;5;75m\x1b[1m12.00 Mbit/s\x1b[0m"),
            "{painted:?}"
        );
        assert!(
            painted.ends_with("\x1b[38;5;210m\x1b[1mStopped delivering data\x1b[0m"),
            "{painted:?}"
        );
        let mut pair = snapshot;
        pair.servers.push(ServerSummary {
            id: "b".into(),
            name: "Beta".into(),
            error: Some("checked".into()),
            ..ServerSummary::default()
        });
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
        assert_eq!(ansi(&[Line::from(span("x", bold))]), "\x1b[1mx\x1b[0m");
    }
}
