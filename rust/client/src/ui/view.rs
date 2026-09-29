//! Go's view.go: the header, the scrolled body and the footer, and the setup, sign-in, chooser
//! and details views.
use super::{
    FRESHNESS, Popup, Prepare, Ui,
    keys::PAGE,
    setup::{ROWS, Setting},
};
use crate::{
    model::ServerSummary,
    report::{Report, Text, cell, fit, line, pad, plain, run_servers, span, truncate, under},
    theme::Theme,
    vocabulary::{self as words, BLOCKED, CHECKING_SIGN_IN, MISSING, NOT_STARTED, START_FAILED},
};
use ratatui::{
    Frame,
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

pub(super) const TWO_COLUMN_MIN: usize = 100;
pub(super) const MIN_WIDTH: usize = 40;
const MIN_HEIGHT: usize = 12;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Go's frame: the header, the body's lines and the rows they get, and the footer.
pub(super) struct Layout {
    top: Text,
    pub body: Text,
    pub body_height: usize,
    footer: Text,
}

/// Go's readiness of a checked server, in the order of its label.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PathState {
    Ready,
    Checking,
    Stale,
    Failed,
    SignIn,
}

impl PathState {
    /// Go's pathLabels.
    fn label(self) -> &'static str {
        ["Ready", "Checking", "Recheck needed", "Failed", "Sign in"][self as usize]
    }
}

/// Go's panel: a rounded frame with its title in the top border; a height pads or clips the body.
pub(super) fn panel(title: &str, mut body: Text, width: usize, height: usize, theme: &Theme) -> Text {
    let inner = width.saturating_sub(4).max(1);
    let title = plain(&fit(Line::from(title.to_owned()), width.saturating_sub(6).max(1)));
    let fill = width.saturating_sub(5 + title.width());
    let mut lines = vec![Line::from(vec![
        span("╭─ ", theme.border),
        span(title, theme.heading),
        span(format!(" {}╮", "─".repeat(fill)), theme.border),
    ])];
    if height > 0 {
        body.truncate(height.saturating_sub(2).max(1));
        body.resize(height.saturating_sub(2), Line::default());
    }
    for text in body {
        let mut row = vec![span("│", theme.border), Span::raw(" ")];
        row.extend(pad(fit(text, inner), inner).spans);
        row.extend([Span::raw(" "), span("│", theme.border)]);
        lines.push(Line::from(row));
    }
    lines.push(line(format!("╰{}╯", "─".repeat(width.saturating_sub(2))), theme.border));
    lines
}

/// Go's join: side by side with a space between, or one above the other.
pub(super) fn join(left: Text, right: Text, side: bool) -> Text {
    if !side {
        return [left, right].concat();
    }
    let width = left.iter().map(Line::width).max().unwrap_or(0);
    let height = left.len().max(right.len());
    let (mut left, mut right) = (left.into_iter(), right.into_iter());
    (0..height)
        .map(|_| {
            let mut row = pad(left.next().unwrap_or_default(), width);
            row.spans.push(Span::raw(" "));
            row.spans.extend(right.next().unwrap_or_default().spans);
            row
        })
        .collect()
}

/// Go's columns: two panels from 100 columns, three fifths to the left.
pub(super) fn columns(width: usize) -> (usize, usize, bool) {
    if width < TWO_COLUMN_MIN {
        return (width, width, false);
    }
    let left = (width - 1) * 3 / 5;
    (left, width - 1 - left, true)
}

/// x/ansi's Wrap (v0.11.8) as lipgloss's Width renders it: words move to the next line, a hyphen
/// may end one, and a word longer than a line breaks.
pub(super) fn wrap(text: &str, limit: usize) -> Vec<String> {
    let (limit, mut out) = (limit.max(1), Wrapped::default());
    for character in text.chars() {
        let cells = cell(character);
        if character.is_whitespace() && character != '\u{a0}' {
            out.add_word();
            out.space.push(character);
            out.space_width += cells;
        } else if character == '-' {
            out.add_space();
            if out.width + out.word_width >= limit {
                out.push(character, cells);
            } else {
                out.add_word();
                out.line.push(character);
                out.width += cells;
            }
        } else if character.is_ascii() {
            if out.width == limit {
                out.newline();
            }
            out.push(character, cells);
            if out.word_width == limit {
                out.add_word();
            }
            out.overflow(limit);
        } else {
            if out.word_width + cells > limit {
                out.add_word();
            }
            out.push(character, cells);
            out.overflow(limit);
            if out.word_width == limit {
                out.add_word();
            }
        }
    }
    if out.word.is_empty() && out.width + out.space_width <= limit {
        out.add_space();
    }
    out.add_word();
    out.newline();
    out.lines
}

/// x/ansi's wrap state: the lines so far, the line being written, and the word and the spaces
/// before it that wait for room.
#[derive(Default)]
struct Wrapped {
    lines: Vec<String>,
    line: String,
    width: usize,
    word: String,
    word_width: usize,
    space: String,
    space_width: usize,
}

impl Wrapped {
    fn push(&mut self, character: char, cells: usize) {
        self.word.push(character);
        self.word_width += cells;
    }

    fn add_space(&mut self) {
        self.line.push_str(&std::mem::take(&mut self.space));
        self.width += std::mem::take(&mut self.space_width);
    }

    fn add_word(&mut self) {
        if !self.word.is_empty() {
            self.add_space();
            self.line.push_str(&std::mem::take(&mut self.word));
            self.width += std::mem::take(&mut self.word_width);
        }
    }

    fn newline(&mut self) {
        self.lines.push(std::mem::take(&mut self.line));
        (self.width, self.space_width) = (0, 0);
        self.space.clear();
    }

    /// A new line once the word and its spaces no longer fit.
    fn overflow(&mut self, limit: usize) {
        if self.width + self.word_width + self.space_width > limit {
            self.newline();
        }
    }
}

impl Ui {
    pub(super) fn spinner(&self) -> Span<'static> {
        span(SPINNER[self.spin % SPINNER.len()], self.theme.accent)
    }

    /// Go's layout in Go's size, the terminal at least 40×12: the header, the view in the body,
    /// and the footer.
    pub(super) fn layout(&self) -> Layout {
        let height = usize::from(self.size.1).max(MIN_HEIGHT);
        let inner = usize::from(self.size.0).max(MIN_WIDTH) - 2;
        let mut top = self.header(inner);
        if height > 24 {
            top.push(Line::default());
        }
        let footer = self.footer(inner, false).len();
        let body_height = height.saturating_sub(top.len() + footer).max(1);
        let theme = &self.theme;
        let body = if self.popup == Popup::Details {
            let details = match self.shown() {
                Some((snapshot, _)) if snapshot.started() => {
                    Report::new(snapshot, self.latency_server(), inner - 4, *theme).details(true)
                }
                _ => vec![line("Waiting for the first server report…", theme.muted)],
            };
            panel("Details", details, inner, 0, theme)
        } else if self.popup == Popup::Servers {
            let (title, content) = self.chooser_view(inner - 4, body_height.saturating_sub(2));
            panel(&title, content, inner, 0, theme)
        } else if self.snapshot.auth.is_some() && self.shown().is_none() {
            let (title, content) = self.sign_in_view();
            let mut body = panel(&title, content, inner, 0, theme);
            body.push(Line::default());
            body.extend(self.sign_in_link(inner));
            body
        } else if self.shown().is_some() {
            self.run_view(inner, body_height)
        } else {
            self.setup_view(inner)
        };
        let footer = self.footer(inner, body.len() > body_height);
        Layout {
            top,
            body,
            body_height,
            footer,
        }
    }

    /// Go's render: the frame one column in from each side, or a request for more room.
    pub(super) fn screen(&self) -> Text {
        let (width, height) = (usize::from(self.size.0), usize::from(self.size.1));
        if width < MIN_WIDTH || height < MIN_HEIGHT {
            let notice = format!("Enlarge the terminal to at least {MIN_WIDTH}×{MIN_HEIGHT}.");
            let lines = wrap(&notice, width.max(1));
            let mut screen = vec![Line::default(); height.saturating_sub(lines.len()) / 2];
            for text in lines {
                let text = text.trim_end().to_owned();
                let indent = width.saturating_sub(text.width()) / 2;
                screen.push(Line::from(format!("{}{text}", " ".repeat(indent))));
            }
            return screen;
        }
        let layout = self.layout();
        let offset = self.body.min(layout.body.len().saturating_sub(layout.body_height));
        let mut body: Text = layout.body.into_iter().skip(offset).take(layout.body_height).collect();
        body.resize(layout.body_height, Line::default());
        let inner = width - 2;
        let mut lines = layout.top;
        // The viewport cuts its lines to its width.
        lines.extend(body.into_iter().map(|line| truncate(line, inner, "")));
        lines.extend(layout.footer);
        for line in &mut lines {
            line.spans.insert(0, Span::raw(" "));
        }
        lines
    }

    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.size = (area.width, area.height);
        let mut lines = self.screen();
        crate::report::sanitize(&mut lines, &self.theme);
        frame.render_widget(Paragraph::new(lines), area);
    }

    /// Go's header: the badge and the status pill, then the version beside the catalogue or the run's servers.
    fn header(&self, width: usize) -> Text {
        let theme = &self.theme;
        let label = self.status_label();
        let (mut pill, stage) = (theme.pill, self.shown().and_then(|(snapshot, _)| snapshot.stage));
        if let Some((snapshot, _)) = self.shown().filter(|_| !self.running()) {
            pill = theme.outcome(snapshot.phase);
        } else if let Some(stage) = stage.filter(|stage| stage.name() == label) {
            pill.bg = theme.stage(stage).fg;
        }
        let (left, right) = (span(" Graphite Meter ", theme.title), span(format!(" {label} "), pill));
        let spacer = width.saturating_sub(left.width() + right.width()).max(1);
        let mut context = self.config.url.clone();
        if let Some((snapshot, _)) = self.shown().filter(|(snapshot, _)| snapshot.started()) {
            let names: Vec<_> = run_servers(snapshot).iter().map(|server| label_of(server)).collect();
            context = names.join(", ");
        }
        let version = span(format!("native client {}  ", crate::VERSION), theme.muted);
        vec![
            fit(Line::from(vec![left, Span::raw(" ".repeat(spacer)), right]), width),
            fit(Line::from(vec![version, span(context, theme.accent)]), width),
        ]
    }

    /// Go's statusLabel: the run's stage or outcome, or setup's state.
    pub(super) fn status_label(&self) -> &'static str {
        if self.live {
            if self.running() {
                return crate::report::status(&self.snapshot);
            }
            if let Some((snapshot, _)) = self.shown() {
                return crate::report::outcome(snapshot.phase);
            }
        }
        if self.config.validate().is_err() {
            BLOCKED
        } else if self.snapshot.auth.is_some() && self.opened {
            CHECKING_SIGN_IN
        } else if self.prepare() == Prepare::SignIn {
            PathState::SignIn.label()
        } else if self.prepare() == Prepare::Failed && self.ready_servers().is_empty() {
            START_FAILED
        } else {
            NOT_STARTED
        }
    }

    /// Go's footer: the notice, an error or the row's help, over the key hints that fit.
    fn footer(&self, width: usize, overflow: bool) -> Text {
        let theme = &self.theme;
        let run_error = self
            .shown()
            .and_then(|(snapshot, _)| snapshot.error.as_deref())
            .filter(|_| !self.running());
        let notice = match &self.edit {
            Some(edit) if !edit.error.is_empty() => line(edit.error.clone(), theme.err),
            _ if run_error.is_some() => line(run_error.unwrap_or_default().to_owned(), theme.err),
            _ if self.notice.is_empty()
                && self.shown().is_none()
                && self.popup == Popup::None
                && self.snapshot.auth.is_none() =>
            {
                line(self.row(self.current()).help, theme.muted)
            }
            _ => line(self.notice.clone(), theme.muted),
        };
        if self.help {
            let mut lines = vec![notice];
            lines.extend(self.full_view(&self.full_help()));
            return lines.into_iter().map(|line| fit(line, width)).collect();
        }
        let mut bindings = self.short_help();
        if overflow && self.popup == Popup::None && self.edit.is_none() && self.snapshot.auth.is_none() {
            bindings.insert(1, PAGE);
        }
        let mut hints = self.short_view(&bindings);
        while hints.width() > width && bindings.len() > 2 {
            bindings.remove(bindings.len() - 2);
            hints = self.short_view(&bindings);
        }
        vec![fit(notice, width), fit(hints, width)]
    }

    /// Go's setupView: the list beside or above the Servers panel.
    fn setup_view(&self, width: usize) -> Text {
        let (left, right, side) = columns(width);
        let (rows, _) = self.setup_list(left - 4);
        let plan = self.plan_view(right - 4);
        let height = if side { rows.len().max(plan.len()) + 2 } else { 0 };
        join(
            panel("Setup", rows, left, height, &self.theme),
            panel("Servers", plan, right, height, &self.theme),
            side,
        )
    }

    /// Go's setupList: the grouped rows, and the line the cursor's row is on.
    pub(super) fn setup_list(&self, width: usize) -> (Text, usize) {
        let rows = self.rows();
        let widest = rows.iter().map(|row| self.row(*row).label.width()).max();
        let label_width = widest.unwrap_or(0).min((width / 2).max(12));
        let (mut lines, mut selected) = (Vec::new(), 0);
        // The rows are ROWS in order, each group after a blank line and its heading.
        for (index, setting) in rows.into_iter().enumerate() {
            if let Some(heading) = ROWS[index].0 {
                lines.extend((index > 0).then(Line::default));
                lines.extend((!heading.is_empty()).then(|| line(heading, self.theme.heading)));
            }
            if index == self.row {
                selected = lines.len();
            }
            lines.push(self.setting_line(setting, index == self.row, label_width, width));
        }
        (lines, selected)
    }

    /// Go's settingLine.
    fn setting_line(&self, setting: Setting, focused: bool, label_width: usize, width: usize) -> Line<'static> {
        let theme = &self.theme;
        let mut line = if setting == Setting::Start {
            let button = if focused {
                vec![span(" Start test ", theme.title)]
            } else {
                vec![Span::raw(" "), span("Start test", theme.heading), Span::raw(" ")]
            };
            Line::from([button, vec![Span::raw("  ")], self.start_note()].concat())
        } else {
            let row = self.row(setting);
            let value = match &self.edit {
                Some(edit) if edit.setting == setting => edit.view(self),
                _ if row.inert => under(row.value, theme.muted),
                _ => under(row.value, theme.value),
            };
            // One span, as Go pads inside the label's style: a focused row's highlight covers the padding.
            let label = plain(&pad(fit(Line::from(row.label), label_width), label_width));
            let mut line = Line::from(span(label, theme.text));
            line.spans.push(Span::raw("  "));
            line.spans.extend(value.spans);
            if focused {
                line = under(fit(line, width.saturating_sub(2)), theme.selected);
            }
            line
        };
        line.spans.insert(0, Span::raw(if focused { "› " } else { "  " }));
        line
    }

    /// Go's startNote: the plan's length, or what keeps the test from starting.
    fn start_note(&self) -> Vec<Span<'static>> {
        let (theme, stages) = (&self.theme, &self.config.stages);
        let total = stages
            .iter()
            .map(|stage| self.config.duration(*stage) + self.config.warmup);
        match self.config.validate() {
            Err(error) => vec![span(error.to_string(), theme.warn)],
            Ok(()) if self.prepare() == Prepare::SignIn => {
                vec![span("sign in first; v requests a new code", theme.warn)]
            }
            Ok(()) if self.prepare() == Prepare::Checking => vec![self.spinner(), span(" checking paths", theme.muted)],
            Ok(()) => {
                let rounded = Duration::from_secs((total.sum::<Duration>().as_secs_f64() + 0.5) as u64);
                let note = format!("{} stages · about {}", stages.len(), words::setting(rounded));
                vec![span(note, theme.muted)]
            }
        }
    }

    /// Go's planView: each checked server's state, what failed, and the paths as checked.
    fn plan_view(&self, width: usize) -> Text {
        let theme = &self.theme;
        let mut lines = Vec::new();
        if self.prepare() == Prepare::Checking && self.prepared.is_empty() {
            lines.push(Line::from(vec![self.spinner(), span(" Checking paths…", theme.muted)]));
        }
        let rows = self.readiness();
        let name_width = rows.iter().map(|(server, ..)| label_of(server).chars().count()).max();
        let name_width = name_width.unwrap_or(0);
        for (server, state, detail) in rows {
            let glyph = match state {
                PathState::Ready => span("●", theme.ok),
                PathState::Checking => self.spinner(),
                PathState::Stale | PathState::SignIn => span("○", theme.warn),
                PathState::Failed => span("✗", theme.err),
            };
            let name = pad(Line::from(label_of(server)), name_width);
            let mut row = vec![glyph, Span::raw(" ")];
            row.extend(name.spans);
            row.extend([Span::raw("  "), span(state.label(), theme.text)]);
            lines.push(Line::from(row));
            if let Some(detail) = detail {
                let wrapped = wrap(&detail, width.max(4).saturating_sub(2));
                lines.extend(wrapped.into_iter().map(|text| line(format!("  {text}"), theme.warn)));
            }
        }
        // Go's prepareFailed error, when no server shows its own.
        let failed = self.prepare() == Prepare::Failed && !self.checked().iter().any(|server| server.error.is_some());
        if let Some(error) = self.check_error.as_deref().filter(|_| failed) {
            lines.extend(wrap(error, width.max(4)).into_iter().map(|text| line(text, theme.warn)));
        }
        if self.can_use_available() && self.snapshot.auth.is_none() {
            lines.push(line("u Use available servers", theme.muted));
        }
        // Go's pathSummaries: each distinct path the check chose, muted once it is no longer fresh,
        // which as Go's FreshFor needs these settings checked lately and every server ready.
        let (mut throughputs, mut latencies) = (Vec::<String>::new(), Vec::<String>::new());
        let mut fresh = self.checked_key == Some(self.config.preparation_key()) && !self.stale();
        for server in self.checked() {
            if !server.checked() || server.error.is_some() {
                fresh = false;
                continue;
            }
            let throughput = server.throughput.as_ref().map(words::throughput_path);
            let latency = server.latency.as_ref().map(words::latency_path);
            for (list, summary) in [(&mut throughputs, throughput), (&mut latencies, latency)] {
                if let Some(summary) = summary.filter(|summary| !list.contains(summary)) {
                    list.push(summary);
                }
            }
        }
        let style = if fresh { theme.value } else { theme.muted };
        let value = |summaries: Vec<String>| match summaries.is_empty() {
            true => span(MISSING, theme.muted),
            false => span(summaries.join(" / "), style),
        };
        lines.push(Line::default());
        lines.push(Line::from(vec![span("Throughput ", theme.text), value(throughputs)]));
        lines.push(Line::from(vec![span("Latency    ", theme.text), value(latencies)]));
        lines
    }

    /// Go's readiness: each checked server's state and the failure to show under it.
    pub(super) fn readiness(&self) -> Vec<(&ServerSummary, PathState, Option<String>)> {
        let checking = self.prepare() == Prepare::Checking;
        self.checked()
            .into_iter()
            .map(|server| {
                let (state, detail) = if checking {
                    (PathState::Checking, None)
                } else if server.sign_in {
                    (PathState::SignIn, None)
                } else if server.error.is_some() || !server.checked() {
                    (PathState::Failed, server.error.clone())
                } else if self.stale() {
                    (PathState::Stale, None)
                } else {
                    (PathState::Ready, None)
                };
                (server, state, detail)
            })
            .collect()
    }

    /// Go's readyServers: those whose paths serve a run, if rechecked.
    pub(super) fn ready_servers(&self) -> Vec<String> {
        let ready = self.readiness().into_iter();
        ready
            .filter(|(_, state, _)| matches!(state, PathState::Ready | PathState::Stale))
            .map(|(server, ..)| server.id.clone())
            .collect()
    }

    /// Go's canUseAvailable: some, not all, servers are ready after a check.
    pub(super) fn can_use_available(&self) -> bool {
        let ready = self.ready_servers().len();
        self.prepare() != Prepare::Checking && ready > 0 && ready < self.readiness().len()
    }

    /// Go's signInView: the code to match and how long the approval waits.
    fn sign_in_view(&self) -> (String, Text) {
        let theme = &self.theme;
        let Some(auth) = &self.snapshot.auth else {
            return (String::new(), Vec::new());
        };
        let challenged = self.snapshot.servers.iter().find(|server| server.sign_in);
        let issuer = challenged.map_or(&self.config.url, |server| &server.name);
        let status = match self.opened {
            true => Line::from(vec![self.spinner(), span(" Waiting for approval…", theme.accent)]),
            false => line("Open the sign-in page below", theme.accent),
        };
        let remaining = auth.deadline.saturating_duration_since(tokio::time::Instant::now());
        let waited = words::clock(crate::net::AUTHORIZATION_TIMEOUT.saturating_sub(remaining));
        let expires = words::setting(Duration::from_secs((remaining.as_secs_f64() + 0.5) as u64));
        // The code in a box of its own, after its label.
        let (label, rule) = ("Match this code ", "─".repeat(auth.code.width() + 2));
        let edge = |corners: [&str; 2]| {
            let edge = span(format!("{}{rule}{}", corners[0], corners[1]), theme.border);
            Line::from(vec![Span::raw(" ".repeat(label.len())), edge])
        };
        let (side, code) = (span("│", theme.border), span(format!(" {} ", auth.code), theme.value));
        let lines = vec![
            status,
            edge(["╭", "╮"]),
            Line::from(vec![span(label, theme.text), side.clone(), code, side]),
            edge(["╰", "╯"]),
            line(format!("waited {waited} · expires in {expires}"), theme.muted),
        ];
        (format!("Sign in to {issuer}"), lines)
    }

    /// Go's signInLink: the page's address outside any frame, hard-wrapped, as a link.
    pub(super) fn sign_in_link(&self, width: usize) -> Text {
        let auth = self.snapshot.auth.iter();
        let url: Vec<char> = auth.flat_map(|auth| auth.browser_url.chars()).collect();
        let lines = url.chunks(width.max(1)).map(String::from_iter);
        lines.map(|text| line(text, self.theme.accent)).collect()
    }

    /// Go's serverChooserView: the catalogue's servers around the cursor, each over its address.
    fn chooser_view(&self, width: usize, height: usize) -> (String, Text) {
        let theme = &self.theme;
        let capacity = (height.saturating_sub(4) / 2).max(2);
        let servers = &self.prepared;
        let last = servers.len().saturating_sub(capacity);
        let start = self.server_row.saturating_sub(capacity / 2).min(last);
        let states = self.readiness();
        let mut lines = Vec::new();
        for (index, server) in servers.iter().enumerate().skip(start).take(capacity) {
            let mut label = format!(" {}", label_of(server));
            if let Some((_, state, _)) = states.iter().find(|(checked, ..)| checked.id == server.id) {
                label = format!("{label} · {}", state.label());
            }
            let (focused, chosen) = (index == self.server_row, self.draft.contains(&server.id));
            let mut row = Line::from(vec![self.checkbox(chosen), Span::raw(label)]);
            if focused {
                row = under(row, theme.selected);
            }
            row.spans.insert(0, Span::raw(if focused { "› " } else { "  " }));
            lines.push(fit(row, width));
            lines.push(fit(line(format!("    {}", server.origin), theme.muted), width));
        }
        (format!("Test servers · {} selected", self.draft.len()), lines)
    }
}

/// Go's serverLabel: the name, then the location.
pub(super) fn label_of(server: &ServerSummary) -> String {
    match server.location.as_str() {
        "" => server.name.clone(),
        location => format!("{} · {location}", server.name),
    }
}

impl Ui {
    /// Go's scrollBody: pages, the ends, or a line.
    pub(super) fn scroll(&mut self, name: &str) {
        let layout = self.layout();
        let bottom = layout.body.len().saturating_sub(layout.body_height);
        let offset = self.body.min(bottom);
        self.body = match name {
            "pgup" => offset.saturating_sub(layout.body_height),
            "pgdown" => (offset + layout.body_height).min(bottom),
            "home" => 0,
            "end" => bottom,
            name if super::keys::reverse(name) => offset.saturating_sub(1),
            _ => (offset + 1).min(bottom),
        };
    }

    /// Go's navigate: the cursor moves within the list, which scrolls to keep it in view.
    pub(super) fn navigate(&mut self, name: &str) {
        let last = self.rows().len() - 1;
        self.row = self.row.saturating_add_signed(super::keys::delta(name)).min(last);
        self.notice.clear();
        let (_, selected) = self.setup_list(usize::from(self.size.0));
        let (layout, line) = (self.layout(), 1 + selected);
        let bottom = layout.body.len().saturating_sub(layout.body_height);
        let offset = self.body.min(bottom);
        let visible = (offset..offset + layout.body_height).contains(&line);
        self.body = if visible { offset } else { line.min(bottom) };
    }

    /// Whether the checked paths are older than Go's preparation freshness.
    pub(super) fn stale(&self) -> bool {
        self.checked_at.is_some_and(|at| at.elapsed() > FRESHNESS)
    }
}
