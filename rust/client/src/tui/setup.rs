//! The setup screen: its rows, what each shows, and the setup and servers panels.
use super::{
    App, Overlay,
    frame::{beside, highlight, unique},
    keys::{self, Binding},
    paths::{LATENCY, Readiness, THROUGHPUT},
};
use crate::{
    config::MAX_STREAMS,
    events::Check,
    model::Stage,
    report::vocabulary as words,
    text::{Line, Style, fill},
};
use std::time::Duration;

/// A setup row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Start,
    Catalogue,
    Servers,
    Throughput,
    Protocol,
    Latency,
    Stage(Stage),
    LoadedLatency,
    Advanced,
    Warmup,
    IdleCadence,
    LoadedCadence,
    ForceStreams,
    Streams,
    Insecure,
    Reset,
}

/// Every row in order: the heading of the group it starts, the row, its label and what the footer says of it.
#[rustfmt::skip]
const ROWS: [(Option<&str>, Row, &str, &str); 19] = [
    (Some(""), Row::Start, "Start test", "Runs the checked stages in order. r starts from any row."),
    (Some("Connection"), Row::Catalogue, "Catalogue URL", "Origin that lists the test servers. enter types one."),
    (None, Row::Servers, "Test servers", "Measured at once; their speeds add up. enter picks up to 4."),
    (None, Row::Throughput, "Throughput path", "How transfers reach the server"),
    (None, Row::Protocol, "HTTP version", "Where the path negotiates. ←/→ Automatic, HTTP/1.1, HTTP/2, HTTP/3."),
    (None, Row::Latency, "Latency path", "How probes travel"),
    (Some("Stages"), Row::Stage(Stage::Latency), "Latency", "Idle round trips"),
    (None, Row::Stage(Stage::Download), "Download", "Server to client"),
    (None, Row::Stage(Stage::Upload), "Upload", "Client to server, receiver-timed"),
    (None, Row::Stage(Stage::Bidirectional), "Bidirectional", "Download and upload at once"),
    (None, Row::LoadedLatency, "Loaded latency", "Round trips during transfers: the latency load adds. space on/off."),
    (Some(""), Row::Advanced, "Advanced", "Warmup, probe cadence, streams and TLS. ←/→ shows or hides."),
    (None, Row::Warmup, "Warmup", "Ramp-up before each window, at least ten round trips. ←/→ ±100 ms (0 ms–4 s)."),
    (None, Row::IdleCadence, "Idle latency cadence", "Probe spacing when idle. ←/→ reply-driven, 80, 250, 600 ms."),
    (None, Row::LoadedCadence, "Loaded latency cadence",
        "Probe spacing during transfers. ←/→ reply-driven, 80, 250, 600 ms."),
    (None, Row::ForceStreams, "Force exact stream count",
        "Off: each path picks its count. On: the count below everywhere."),
    (None, Row::Streams, "Maximum H1 streams per direction", "Upper bound on HTTP/1.1 paths. ←/→ ±1 (1–14)."),
    (None, Row::Insecure, "Skip TLS verify", "Accepts any certificate. Unsafe; sign-in is refused. space on/off."),
    (None, Row::Reset, "Reset settings", "Restores defaults; keeps the catalogue and servers."),
];

impl Row {
    /// The row's label and what the footer says of it.
    fn text(self) -> (&'static str, &'static str) {
        let row = ROWS.iter().find(|(_, row, ..)| *row == self);
        row.map(|(.., label, help)| (*label, *help)).unwrap_or_default()
    }

    pub(super) fn label(self) -> &'static str {
        self.text().0
    }

    /// What enter does on the row.
    fn verb(self) -> &'static str {
        match self {
            Self::Servers | Self::Advanced => "open",
            Self::Reset => "reset",
            Self::Catalogue | Self::Streams | Self::Stage(_) | Self::Warmup => "edit",
            Self::LoadedLatency | Self::Insecure => "on/off",
            _ => "next",
        }
    }

    fn adjusts(self) -> bool {
        !matches!(self, Self::Start | Self::Catalogue | Self::Servers | Self::Reset)
    }
}

/// Where setup stands: the focused row, the advanced rows shown, a reset awaiting its confirmation, and the chooser
/// waiting for the check in progress.
#[derive(Debug, Default)]
pub struct Setup {
    pub row: usize,
    pub advanced: bool,
    pub resetting: bool,
    pub chooser: bool,
}

/// A row as setup shows it.
pub struct Shown {
    label: &'static str,
    pub value: Line,
    pub help: String,
}

impl App {
    pub(super) fn rows(&self) -> Vec<Row> {
        let rows = ROWS.iter().map(|(_, row, ..)| *row);
        let advanced = rows.clone().position(|row| row == Row::Advanced).unwrap_or_default();
        let shown = if self.setup.advanced { ROWS.len() } else { advanced + 1 };
        rows.take(shown).collect()
    }

    pub(super) fn row(&self) -> Row {
        let rows = self.rows();
        rows[self.setup.row.min(rows.len() - 1)]
    }

    pub(super) fn first_stage(&self) -> usize {
        let stage = self.rows().iter().position(|row| matches!(row, Row::Stage(_)));
        stage.unwrap_or(0)
    }

    pub(super) fn flag(&self, row: Row) -> Option<bool> {
        match row {
            Row::Stage(stage) => Some(self.config.stages.contains(&stage)),
            Row::LoadedLatency => Some(self.config.loaded_latency),
            Row::Insecure => Some(self.config.insecure),
            _ => None,
        }
    }

    pub(super) fn set_flag(&mut self, row: Row, on: bool) {
        match row {
            Row::Stage(stage) => {
                self.config.stages.retain(|kept| *kept != stage);
                self.config.stages.extend(on.then_some(stage));
                self.config.stages.sort_unstable();
            }
            Row::LoadedLatency => self.config.loaded_latency = on,
            _ => self.config.insecure = on,
        }
        self.notice = format!("{} {}.", row.label(), if on { "on" } else { "off" });
    }

    pub(super) fn show(&self, row: Row) -> Shown {
        let (config, palette) = (&self.config, &self.palette);
        let (mut label, help) = row.text();
        let (mut help, mut inert) = (help.to_owned(), false);
        let (checkbox, text) = match row {
            Row::Start | Row::Reset => (None, String::new()),
            Row::Catalogue => (None, config.url.to_string()),
            Row::Servers => {
                inert = self.view.catalogue.len() < 2;
                (None, self.selection())
            }
            Row::Throughput => (None, self.path_value(&THROUGHPUT, &mut help)),
            Row::Latency => (None, self.path_value(&LATENCY, &mut help)),
            Row::Protocol => match self.fixed_protocol() {
                Some(fixed) => {
                    (help, inert) = ("Fixed by this path; pick another path to change it.".into(), true);
                    (None, words::protocol(Some(fixed)).into())
                }
                None => (None, words::protocol(config.paths.protocol).into()),
            },
            Row::Stage(stage) => {
                inert = !config.stages.contains(&stage);
                help.push_str(". ←/→ step it (1 s–24 h; a server may allow less), space on/off.");
                (Some(!inert), words::setting(config.duration(stage)))
            }
            Row::LoadedLatency => (Some(config.loaded_latency), String::new()),
            Row::Advanced => (None, if self.setup.advanced { "▾ shown" } else { "▸ hidden" }.into()),
            Row::Warmup => (None, words::setting(config.warmup)),
            Row::IdleCadence => (None, words::cadence(config.ping)),
            Row::LoadedCadence => (None, words::cadence(config.loaded_ping)),
            Row::ForceStreams => (Some(config.streams.forced > 0), String::new()),
            Row::Streams => {
                if config.streams.forced > 0 {
                    label = "Streams per server and direction";
                    help = format!("Exact streams per server and direction. ←/→ ±1 (1–{MAX_STREAMS}).");
                }
                let count = if config.streams.forced > 0 {
                    config.streams.forced
                } else {
                    config.streams.auto
                };
                (None, count.to_string())
            }
            Row::Insecure => (Some(config.insecure), String::new()),
        };
        let style = if inert { palette.muted } else { palette.value };
        let value = match checkbox {
            Some(on) if text.is_empty() => self.checkbox(on),
            Some(on) => self.checkbox(on).and(format!(" {text}"), style),
            None => Line::styled(text, style),
        };
        Shown { label, value, help }
    }

    /// The selected servers' names and how many are ready.
    fn selection(&self) -> String {
        let names: Vec<_> = self.view.servers.iter().map(|server| server.name.as_str()).collect();
        let (servers, ready) = (names.len(), self.ready().len());
        let summary = match () {
            _ if servers == 0 => String::new(),
            _ if self.checking() => " · checking".into(),
            _ if ready == servers => " · ready".into(),
            _ => format!(" · {ready} of {servers} ready"),
        };
        match names.is_empty() {
            true => words::MISSING.into(),
            false => names.join(", ") + &summary,
        }
    }

    /// The footer's keys for the focused row.
    pub(super) fn hints(&self) -> Vec<Binding> {
        let row = self.row();
        if row == Row::Start {
            return vec![keys::OPEN.does("start test"), keys::MOVE, keys::HELP, keys::QUIT];
        }
        let mut hints = vec![keys::START, keys::MOVE];
        hints.extend(row.adjusts().then_some(keys::ADJUST));
        hints.extend(matches!(row, Row::Stage(_)).then_some(keys::TOGGLE));
        hints.extend([keys::OPEN.does(row.verb()), keys::HELP, keys::QUIT]);
        hints
    }

    /// The setup list, and the line of its focused row.
    pub(super) fn setup_list(&self, width: usize) -> (Vec<Line>, usize) {
        let rows = self.rows();
        let shown: Vec<_> = rows.iter().map(|&row| self.show(row)).collect();
        let labels = shown.iter().map(|shown| crate::text::width(shown.label)).max();
        let label_width = labels.unwrap_or(0).min((width / 2).max(12));
        let (mut lines, mut focus) = (Vec::new(), 0);
        for (index, ((heading, row, ..), shown)) in ROWS.iter().zip(&shown).enumerate() {
            if let Some(heading) = heading {
                lines.extend((index > 0).then(Line::default));
                lines.extend((!heading.is_empty()).then(|| Line::styled(*heading, self.palette.heading)));
            }
            let focused = index == self.setup.row;
            focus = if focused { lines.len() } else { focus };
            lines.push(self.setting(*row, shown, focused, label_width, width));
        }
        (lines, focus)
    }

    fn setting(&self, row: Row, shown: &Shown, focused: bool, label_width: usize, width: usize) -> Line {
        let palette = &self.palette;
        let label = Line::styled(shown.label, palette.text)
            .fit(label_width)
            .pad(label_width);
        let label = label.and("  ", Style::default());
        let line = match (row, &self.overlay) {
            (Row::Start, _) => {
                let button = Line::styled(" Start test ", if focused { palette.title } else { palette.heading });
                button.and("  ", Style::default()).with(self.start_note())
            }
            (_, Overlay::Edit(editor)) if editor.row == row => {
                label.with(editor.line(width.saturating_sub(label_width + 5), palette))
            }
            _ if focused => highlight(label.with(shown.value.clone()).fit(width.saturating_sub(2)), palette.selected),
            _ => label.with(shown.value.clone()),
        };
        Line::plain(if focused { "› " } else { "  " }).with(line)
    }

    /// Beside Start test: why the settings cannot run, the check in progress, or the plan's length.
    fn start_note(&self) -> Line {
        let palette = &self.palette;
        if let Err(error) = self.config.validate() {
            return Line::styled(error, palette.warn);
        }
        if self.view.check == Check::SignIn {
            return Line::styled("sign in first; v requests a new code", palette.warn);
        }
        if self.checking() {
            return Line::styled(self.spinner(), palette.accent).and(" checking paths", palette.muted);
        }
        let plan = self.config.plan();
        let total: Duration = plan.iter().map(|(_, duration)| *duration + self.config.warmup).sum();
        let about = words::setting(Duration::from_secs(total.as_secs_f64().round() as u64));
        Line::styled(format!("{} stages · about {about}", plan.len()), palette.muted)
    }

    /// The setup and servers panels, side by side when wide enough, and the focused row's line.
    pub(super) fn setup_body(&self, width: usize) -> (Vec<Line>, usize) {
        let side = columns(width);
        let (left, right) = side.unwrap_or((width, width));
        let (rows, focus) = self.setup_list(left.saturating_sub(4));
        let servers = self.servers_panel(right.saturating_sub(4));
        let height = if side.is_some() { rows.len().max(servers.len()) + 2 } else { 0 };
        let (rows, servers) = (self.panel("Setup", rows, left, height), self.panel("Servers", servers, right, height));
        match side {
            Some(_) => (beside(rows, servers), focus + 1),
            None => ([rows, servers].concat(), focus + 1),
        }
    }

    /// Each selected server's readiness, why the check failed, and the paths it found.
    fn servers_panel(&self, width: usize) -> Vec<Line> {
        let (palette, servers) = (&self.palette, &self.view.servers);
        let mut lines = Vec::new();
        if self.checking() && servers.is_empty() {
            lines.push(self.checking_line());
        }
        let warn = |text: &str, indent: &'static str| {
            let filled = fill(text, width.saturating_sub(indent.len()).max(4)).into_iter();
            filled.map(move |line| Line::plain(indent).and(line, palette.warn))
        };
        let names = self.labels();
        let name_width = names.iter().map(|name| crate::text::width(name)).max().unwrap_or(0);
        for (server, name) in servers.iter().zip(names) {
            let state = self.readiness(server);
            let glyph = match state {
                Readiness::Ready => Line::styled("●", palette.ok),
                Readiness::Checking => Line::styled(self.spinner(), palette.accent),
                Readiness::Stale | Readiness::SignIn => Line::styled("○", palette.warn),
                Readiness::Failed => Line::styled("✗", palette.err),
            };
            let name = Line::plain(name).pad(name_width);
            lines.push(
                glyph
                    .and(" ", Style::default())
                    .with(name)
                    .and("  ", Style::default())
                    .and(state.label(), palette.text),
            );
            if let (Readiness::Failed, Err(failure)) = (state, &server.path) {
                lines.extend(warn(&failure.text, "  "));
            }
        }
        if let Check::Failed(failure) = &self.view.check {
            lines.extend(warn(&failure.text, ""));
        }
        if self.can_use_available() {
            lines.push(Line::styled("u Use available servers", palette.muted));
        }
        let style = if self.fresh() { palette.value } else { palette.muted };
        let summary = |labels: Vec<String>| match labels.is_empty() {
            true => Line::styled(words::MISSING, palette.muted),
            false => Line::styled(unique(labels).join(" / "), style),
        };
        let paths = servers.iter().filter_map(|server| server.path.as_ref().ok());
        let throughput = paths.clone().map(|paths| words::throughput_path(&paths.throughput));
        let latency = paths.filter_map(|paths| paths.latency.as_ref().map(words::latency_path));
        lines.push(Line::default());
        lines.push(Line::styled("Throughput ", palette.text).with(summary(throughput.collect())));
        lines.push(Line::styled("Latency    ", palette.text).with(summary(latency.collect())));
        lines
    }
}

/// The left and right panels' widths, side by side from 100 cells.
pub(super) fn columns(width: usize) -> Option<(usize, usize)> {
    let left = width.saturating_sub(1) * 3 / 5;
    (width >= 100).then(|| (left, width - 1 - left))
}
