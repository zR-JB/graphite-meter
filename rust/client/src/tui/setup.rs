//! The settings sheet's rows: what each shows and says, grouped as the browser's settings are.
use super::{
    App, Overlay, Target,
    frame::Entry,
    paths::{LATENCY, THROUGHPUT},
};
use crate::{
    model::Stage,
    report::vocabulary as words,
    text::{Line, Style},
};
use std::time::Duration;

/// A settings row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Catalogue,
    Remember,
    Servers,
    Throughput,
    Protocol,
    Latency,
    Stage(Stage),
    LoadedLatency,
    Warmup,
    IdleCadence,
    LoadedCadence,
    ForceStreams,
    Streams,
    Insecure,
    Reset,
}

/// Every row in order: the heading of the group it starts, the row, its label and what the sheet's foot says of it.
#[rustfmt::skip]
const ROWS: [(Option<&str>, Row, &str, &str); 18] = [
    (Some("Server"), Row::Catalogue, "Address",
        "The Graphite Meter server to test, which lists its test servers. enter edits it; a pasted address works too."),
    (None, Row::Remember, "Remember server", "Keeps the address on this computer, so the next start needs none. Off deletes it."),
    (None, Row::Servers, "Test servers", "Measured at once; their speeds add up. enter picks up to 4."),
    (None, Row::Throughput, "Throughput path", "How transfers reach the server."),
    (None, Row::Protocol, "HTTP version", "Where the path negotiates. ←→ Automatic, HTTP/1.1, HTTP/2, HTTP/3."),
    (None, Row::Latency, "Latency path", "How probes travel."),
    (Some("Stages"), Row::Stage(Stage::Latency), "Latency", "Idle round trips."),
    (None, Row::Stage(Stage::Download), "Download", "Server to client."),
    (None, Row::Stage(Stage::Upload), "Upload", "Client to server, timed by the server as it receives."),
    (None, Row::Stage(Stage::Bidirectional), "Bidirectional", "Download and upload at once."),
    (None, Row::LoadedLatency, "Loaded latency", "Round trips during transfers: the latency the load adds."),
    (Some("Advanced"), Row::Warmup, "Warmup",
        "Ramp-up before each window, at least ten round trips. ←→ ±100 ms, from 0 ms to 4 s."),
    (None, Row::IdleCadence, "Idle probe spacing", "How often idle latency is probed: on each reply, or every 80, 250 or 600 ms."),
    (None, Row::LoadedCadence, "Loaded probe spacing", "How often latency is probed during transfers."),
    (None, Row::ForceStreams, "Exact stream count", "Off: each path picks its own. On: the count below everywhere."),
    (None, Row::Streams, "HTTP/1.1 streams", "The most HTTP/1.1 connections per direction. ←→ ±1, from 1 to 14."),
    (None, Row::Insecure, "Skip TLS verification", "Accepts any certificate. Unsafe, and sign-in is refused."),
    (None, Row::Reset, "Reset settings", "Restores the defaults; keeps the server and its test servers."),
];

impl Row {
    /// The row's label and what the sheet's foot says of it.
    fn text(self) -> (&'static str, &'static str) {
        let row = ROWS.iter().find(|(_, row, ..)| *row == self);
        row.map(|(.., label, help)| (*label, *help)).unwrap_or_default()
    }

    pub(super) fn label(self) -> &'static str {
        self.text().0
    }
}

/// Where the settings sheet stands: its focused row, a reset awaiting confirmation, the chooser awaiting the check.
#[derive(Debug, Default)]
pub struct Setup {
    pub row: usize,
    pub resetting: bool,
    pub chooser: bool,
}

impl App {
    pub(super) fn row(&self) -> Row {
        ROWS[self.setup.row.min(ROWS.len() - 1)].1
    }

    /// Opens the settings sheet, focused on `row` when given.
    pub(super) fn open_settings(&mut self, row: Option<Row>) {
        if let Some(at) = row.and_then(|row| ROWS.iter().position(|(_, own, ..)| *own == row)) {
            self.setup.row = at;
        }
        (self.overlay, self.editor, self.scroll) = (Overlay::Settings, None, 0);
    }

    /// Moves the focus `step` rows.
    pub(super) fn move_focus(&mut self, step: isize) {
        self.setup.row = self.setup.row.saturating_add_signed(step).min(ROWS.len() - 1);
        self.setup.resetting = false;
    }

    pub(super) fn flag(&self, row: Row) -> Option<bool> {
        match row {
            Row::Stage(stage) => Some(self.config.stages.contains(&stage)),
            Row::LoadedLatency => Some(self.config.loaded_latency),
            Row::ForceStreams => Some(self.config.streams.forced > 0),
            Row::Insecure => Some(self.config.insecure),
            Row::Remember => Some(self.remembering),
            _ => None,
        }
    }

    /// A row's value as the sheet shows it, greyed while it does nothing.
    fn value(&self, row: Row) -> Line {
        let (config, palette) = (&self.config, &self.palette);
        let mut inert = false;
        let text = match row {
            Row::Catalogue => config
                .url
                .as_ref()
                .map_or_else(|| "none yet".into(), ToString::to_string),
            Row::Servers => {
                inert = self.view.catalogue.len() < 2;
                self.selection()
            }
            Row::Throughput => self.path_value(&THROUGHPUT),
            Row::Latency => self.path_value(&LATENCY),
            Row::Protocol => match self.fixed_protocol() {
                Some(fixed) => {
                    inert = true;
                    words::protocol(Some(fixed)).into()
                }
                None => words::protocol(config.paths.protocol).into(),
            },
            Row::Stage(stage) => {
                inert = !config.stages.contains(&stage);
                words::setting(config.duration(stage))
            }
            Row::Warmup => words::setting(config.warmup),
            Row::IdleCadence => words::cadence(config.ping),
            Row::LoadedCadence => words::cadence(config.loaded_ping),
            Row::Streams if config.streams.forced > 0 => config.streams.forced.to_string(),
            Row::Streams => config.streams.auto.to_string(),
            Row::Reset if self.setup.resetting => return Line::styled("enter again resets", palette.warn),
            Row::Remember | Row::LoadedLatency | Row::ForceStreams | Row::Insecure | Row::Reset => String::new(),
        };
        let style = if inert { palette.muted } else { palette.value };
        match self.flag(row) {
            Some(on) if text.is_empty() => self.switch_mark(on),
            Some(on) => Line::styled(text, style)
                .and("  ", Style::default())
                .with(self.switch_mark(on)),
            None => Line::styled(text, style),
        }
    }

    /// A switch: on in ink, off as a quiet ring.
    pub(super) fn switch_mark(&self, on: bool) -> Line {
        match on {
            true => Line::styled("● on ", self.palette.accent),
            false => Line::styled("○ off", self.palette.muted),
        }
    }

    /// The sheet's entries and the focused row's index among them.
    pub(super) fn settings_entries(&self) -> (Vec<Entry>, usize) {
        let (mut entries, mut focus) = (Vec::new(), 0);
        for (index, &(heading, row, label, _)) in ROWS.iter().enumerate() {
            if let Some(heading) = heading {
                entries.push(Entry::Heading(heading.into()));
            }
            if index == self.setup.row {
                focus = entries.len();
            }
            let label = match row {
                Row::Streams if self.config.streams.forced > 0 => "Streams per direction",
                _ => label,
            };
            let value = match &self.editor {
                Some(editor) if editor.row == row => editor.line(40, &self.palette),
                _ => self.value(row),
            };
            let label = Line::styled(label, self.palette.text);
            entries.push(Entry::Item { label, value, target: Some(Target::Row(index)) });
        }
        (entries, focus)
    }

    /// What the sheet's foot says: the editor's error, or what the focused row does.
    pub(super) fn settings_footer(&self) -> Vec<Line> {
        if let Some(error) = self.editor.as_ref().and_then(|editor| editor.error.clone()) {
            return vec![Line::styled(error, self.palette.warn)];
        }
        let row = self.row();
        let mut help = row.text().1.to_owned();
        match row {
            Row::Throughput | Row::Latency => {
                let kind = if row == Row::Throughput {
                    self.choices(&THROUGHPUT).len()
                } else {
                    self.choices(&LATENCY).len()
                };
                help.push_str(&format!(" ←→ picks one of {kind}."));
            }
            Row::Protocol if self.fixed_protocol().is_some() => {
                help = "Fixed by this path; pick another path to change it.".into()
            }
            Row::Stage(_) => help.push_str(" ←→ steps its time, space switches it."),
            Row::LoadedLatency | Row::Insecure | Row::ForceStreams => help.push_str(" space switches it."),
            _ => {}
        }
        vec![Line::styled(help, self.palette.muted)]
    }

    /// The selected servers' names and how many are ready.
    fn selection(&self) -> String {
        let names: Vec<_> = self.view.servers.iter().map(|server| server.name.as_str()).collect();
        let (servers, ready) = (names.len(), self.ready().len());
        let summary = match () {
            _ if servers == 0 => String::new(),
            _ if self.checking() => " · checking".into(),
            _ if ready == servers => String::new(),
            _ => format!(" · {ready} of {servers} ready"),
        };
        match names.is_empty() {
            true => words::MISSING.into(),
            false => names.join(", ") + &summary,
        }
    }

    /// How long the configured stages take with their warmups.
    pub(super) fn plan_time(&self) -> String {
        let plan = self.config.plan();
        let total: Duration = plan.iter().map(|(_, duration)| *duration + self.config.warmup).sum();
        words::setting(Duration::from_secs(total.as_secs_f64().round() as u64))
    }
}
