//! Lays out the settings sheet's rows, what each shows and says, grouped as the browser's settings are.
use super::{
    App, Overlay, Target,
    frame::Entry,
    paths::{LATENCY, THROUGHPUT},
};
use crate::{
    config::{MAX_STREAMS, MAX_WARMUP},
    format::{self, MISSING},
    model::Stage,
    text::{Line, Style},
};
use graphite_meter_proto::catalog::MAX_SELECTED;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Address,
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

/// Each row in order carries the heading of the group it starts, the row, its label and what the sheet's foot says.
#[rustfmt::skip]
const ROWS: [(Option<&str>, Row, &str, &str); 18] = [
    (Some("Server"), Row::Address, "Server address",
        "The Graphite Meter server to test, which lists its test servers. Enter edits it; a pasted address works too."),
    (None, Row::Remember, "Remember this server",
        "Keeps the address on this computer, so the next start needs none. Off deletes it."),
    (None, Row::Servers, "Test servers", "Measured at once; their speeds add up."),
    (None, Row::Throughput, "Throughput path", "How transfers reach the server."),
    (None, Row::Protocol, "HTTP version", "Where the path negotiates. ←→ Automatic, HTTP/1.1, HTTP/2, HTTP/3."),
    (None, Row::Latency, "Latency path", "How probes travel."),
    (Some("Stages"), Row::Stage(Stage::Latency), "Latency", "Idle round trips."),
    (None, Row::Stage(Stage::Download), "Download", "Server to client."),
    (None, Row::Stage(Stage::Upload), "Upload", "Client to server, timed by the server as it receives."),
    (None, Row::Stage(Stage::Bidirectional), "Bidirectional", "Download and upload at once."),
    (None, Row::LoadedLatency, "Loaded latency", "Round trips during transfers: the latency the load adds."),
    (Some("Advanced"), Row::Warmup, "Warmup", "Opens the connections before each stage; at least 10 round trips"),
    (None, Row::IdleCadence, "Idle probe interval",
        "How often idle latency is probed: on each reply, or at a fixed interval."),
    (None, Row::LoadedCadence, "Loaded probe interval", "How often latency is probed during transfers."),
    (None, Row::ForceStreams, "Exact stream count", "Off: each path picks its own. On: the count below everywhere."),
    (None, Row::Streams, "HTTP/1.1 stream limit", "The most HTTP/1.1 streams per direction."),
    (None, Row::Insecure, "Skip TLS verification", "Accepts any certificate. Unsafe, and sign-in is refused."),
    (None, Row::Reset, "Reset settings", "Restores the defaults; keeps the server and its test servers."),
];

impl Row {
    fn text(self) -> (&'static str, &'static str) {
        let row = ROWS.iter().find(|(_, row, ..)| *row == self);
        row.map(|(.., label, help)| (*label, *help))
            .unwrap_or_default()
    }

    pub(super) fn label(self) -> &'static str {
        self.text().0
    }
}

/// Tracks the settings sheet's focused row, and whether the chooser opens once the check in progress ends.
#[derive(Debug, Default)]
pub struct Setup {
    pub row: usize,
    pub chooser: bool,
}

impl App {
    pub(super) fn row(&self) -> Row {
        ROWS[self.setup.row.min(ROWS.len() - 1)].1
    }

    pub(super) fn open_settings(&mut self, row: Option<Row>) {
        if let Some(at) = row.and_then(|row| ROWS.iter().position(|(_, own, ..)| *own == row)) {
            self.setup.row = at;
        }
        (self.overlay, self.scroll) = (Overlay::SETTINGS, 0);
    }

    pub(super) fn move_focus(&mut self, step: isize) {
        self.setup.row = self
            .setup
            .row
            .saturating_add_signed(step)
            .min(ROWS.len() - 1);
        self.resetting(false);
    }

    pub(super) fn resetting(&mut self, awaits: bool) -> bool {
        match &mut self.overlay {
            Overlay::Settings { resetting, .. } => std::mem::replace(resetting, awaits),
            _ => false,
        }
    }

    pub(super) fn flag(&self, row: Row) -> Option<bool> {
        match row {
            Row::Stage(stage) => Some(self.config.stages.contains(&stage)),
            Row::LoadedLatency => Some(self.config.loaded_latency),
            Row::ForceStreams => Some(self.config.streams.forced.is_some()),
            Row::Insecure => Some(self.config.insecure),
            Row::Remember => Some(self.remembering),
            _ => None,
        }
    }

    /// Shows a row's value as the sheet does, greyed while it does nothing.
    fn value(&self, row: Row) -> Line {
        let (config, palette) = (&self.config, &self.palette);
        let mut inert = false;
        let text = match row {
            Row::Address => config
                .url
                .as_ref()
                .map_or_else(|| "none yet".into(), ToString::to_string),
            Row::Servers => {
                inert = self.view.catalog.len() < 2;
                self.selection()
            }
            Row::Throughput => self.path_value(&THROUGHPUT),
            Row::Latency => self.path_value(&LATENCY),
            Row::Protocol => match self.fixed_protocol() {
                Some(fixed) => {
                    inert = true;
                    format::protocol(Some(fixed)).into()
                }
                None => format::protocol(config.paths.protocol).into(),
            },
            Row::Stage(stage) => {
                inert = !config.stages.contains(&stage);
                format::stage_time(config.duration(stage))
            }
            Row::Warmup => format::stage_time(config.warmup),
            Row::IdleCadence => format::cadence(config.ping),
            Row::LoadedCadence => format::cadence(config.loaded_ping),
            Row::Streams => config.streams.shown().to_string(),
            Row::Reset
                if matches!(
                    self.overlay,
                    Overlay::Settings {
                        resetting: true,
                        ..
                    }
                ) =>
            {
                return Line::styled("Enter again resets", palette.warn);
            }
            Row::Remember | Row::LoadedLatency | Row::ForceStreams | Row::Insecure | Row::Reset => {
                String::new()
            }
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

    /// A switch shows on in ink and off as a quiet ring.
    pub(super) fn switch_mark(&self, on: bool) -> Line {
        match on {
            true => Line::styled("● on ", self.palette.accent),
            false => Line::styled("○ off", self.palette.muted),
        }
    }

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
                Row::Streams if self.config.streams.forced.is_some() => "Streams per direction",
                _ => label,
            };
            let value = match self.editor() {
                Some(editor) if editor.row == row => editor.line(40, &self.palette),
                _ => self.value(row),
            };
            let label = Line::styled(label, self.palette.text);
            entries.push(Entry::Item {
                label,
                value,
                target: Some(Target::Row(index)),
            });
        }
        (entries, focus)
    }

    /// The sheet's foot says the editor's error, or what the focused row does.
    pub(super) fn settings_footer(&self) -> Vec<Line> {
        if let Some(error) = self.editor().and_then(|editor| editor.error.clone()) {
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
                help = "Fixed by this path; pick another path to change it.".into();
            }
            Row::Servers => help.push_str(&format!(" Enter picks up to {MAX_SELECTED}.")),
            Row::Stage(_) => help.push_str(" ←→ steps its time; Space switches it."),
            Row::Warmup => {
                help.push_str(&format!(
                    ", up to {}. Never counted. ←→ ±100 ms.",
                    format::stage_time(MAX_WARMUP)
                ));
            }
            Row::Streams if self.config.streams.forced.is_some() => {
                help = format!(
                    "The streams every path opens per direction. ←→ ±1, from 1 to {MAX_STREAMS}."
                );
            }
            Row::Streams => help.push_str(&format!(" ←→ ±1, from 1 to {MAX_STREAMS}.")),
            Row::LoadedLatency | Row::Insecure | Row::ForceStreams => {
                help.push_str(" Space switches it.");
            }
            _ => {}
        }
        vec![Line::styled(help, self.palette.muted)]
    }

    fn selection(&self) -> String {
        let names: Vec<_> = self
            .view
            .servers
            .iter()
            .map(|server| server.name.as_str())
            .collect();
        let (servers, ready) = (names.len(), self.ready().len());
        let summary = match () {
            _ if servers == 0 => String::new(),
            _ if self.checking() => " · checking".into(),
            _ if ready == servers => String::new(),
            _ => format!(" · {ready} of {servers} ready"),
        };
        match names.is_empty() {
            true => MISSING.into(),
            false => names.join(", ") + &summary,
        }
    }

    pub(super) fn plan_time(&self) -> String {
        let plan = self.config.plan();
        let total: Duration = plan
            .iter()
            .map(|(_, duration)| *duration + self.config.warmup)
            .sum();
        format::stage_time(Duration::from_secs(total.as_secs_f64().round() as u64))
    }
}
