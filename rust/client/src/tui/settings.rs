//! What setup's keys change: steps, cycles, flags, resets and the inline editor.
use super::{
    App, Effect, Overlay, RECHECK,
    keys::{self, Action, Key},
    paths::{LATENCY, THROUGHPUT},
    setup::Row,
};
use crate::{
    config::{Config, MAX_STREAMS, PathChoice, Streams, server_origin},
    events::Check,
    report::vocabulary::{self as words, CADENCES},
    text::Line,
    tui::theme::Palette,
};
use crossterm::event::KeyCode;
use graphite_meter_proto::{
    discovery::{Protocol, STAGE_LIMITS},
    duration,
    text::safe,
};
use std::{ops::RangeInclusive, time::Duration};
use unicode_width::UnicodeWidthChar;

/// The most characters the editor holds.
const MAX_TEXT: usize = 4096;
const WARMUP: RangeInclusive<Duration> = Duration::ZERO..=Duration::from_secs(4);

impl App {
    /// Reacts to `action` on the focused row; a change to what a check depends on rechecks the paths.
    pub(super) fn setup_key(&mut self, action: Action, key: Key) -> Vec<Effect> {
        let (row, step) = (self.row(), key.step());
        let flag = self.flag(row);
        let action = if action == Action::Toggle && flag.is_none() { Action::Open } else { action };
        let resetting = std::mem::take(&mut self.setup.resetting);
        if resetting && (action, row) != (Action::Open, Row::Reset) {
            self.notice = "Settings kept.".into();
            return Vec::new();
        }
        if action == Action::Start && self.view.check == Check::SignIn {
            self.notice = "Test cannot start: sign in first. Press v to request a new code.".into();
            return Vec::new();
        }
        if action == Action::Start || (action, row) == (Action::Open, Row::Start) {
            return self.start();
        }
        let before = self.config.key();
        match action {
            Action::Move => {
                let last = self.rows().len() - 1;
                self.setup.row = self.setup.row.saturating_add_signed(step).min(last);
                (self.notice, self.follow) = (String::new(), true);
            }
            Action::Adjust => self.adjust(row, step),
            Action::Toggle => self.set_flag(row, flag == Some(false)),
            Action::Open if row == Row::Reset => self.reset(resetting),
            Action::Open => self.activate(row),
            Action::Recheck => self.recheck_soon(),
            Action::Servers => self.open_chooser(),
            Action::Available => {
                (self.config.servers, self.notice) = (self.ready(), "Using the available servers.".into());
                self.recheck_soon();
            }
            Action::Automatic => {
                self.config.paths = PathChoice::default();
                self.notice = "Automatic paths applied to every selected server.".into();
                self.recheck_soon();
            }
            _ => {}
        }
        if self.config.key() != before {
            self.recheck_soon();
        }
        Vec::new()
    }

    /// Checks the paths once the settings rest; without a server there are none to check.
    pub(super) fn recheck_soon(&mut self) {
        self.recheck = self.config.url.as_ref().map(|_| self.now + RECHECK);
    }

    fn reset(&mut self, confirmed: bool) {
        if !confirmed {
            self.setup.resetting = true;
            self.notice = "Press enter again to reset every setting; any other key keeps them.".into();
            return;
        }
        let (url, servers) = (self.config.url.clone(), std::mem::take(&mut self.config.servers));
        self.config = Config { url, servers, ..Config::default() };
        self.notice = "Settings reset to defaults.".into();
    }

    /// Enter or space on `row`.
    fn activate(&mut self, row: Row) {
        match row {
            Row::Servers => self.open_chooser(),
            Row::Stage(_) | Row::Warmup => {
                let value = span(&mut self.config, row).map(|(value, ..)| duration::format(*value));
                self.edit(row, &value.unwrap_or_default());
            }
            Row::Catalogue => self.edit(row, &self.config.url.as_ref().map(ToString::to_string).unwrap_or_default()),
            Row::Streams => self.edit(row, &self.show(row).value.text()),
            _ => match self.flag(row) {
                Some(on) => self.set_flag(row, !on),
                None => self.cycle(row, 1),
            },
        }
    }

    /// ←/→ on `row`: a duration steps, a flag turns on or off, anything else cycles.
    fn adjust(&mut self, row: Row, step: isize) {
        if let Some((value, bounds, _)) = span(&mut self.config, row) {
            let unit = match value
                .saturating_sub(Duration::from_nanos(u64::from(step < 0)))
                .as_secs()
            {
                _ if row == Row::Warmup => Duration::from_millis(100),
                0..60 => Duration::from_secs(1),
                60..600 => Duration::from_secs(10),
                600..3600 => Duration::from_secs(60),
                _ => Duration::from_secs(300),
            };
            let moved = if step > 0 { value.saturating_add(unit) } else { value.saturating_sub(unit) };
            *value = moved.clamp(*bounds.start(), *bounds.end());
            self.notice = format!("{} {}.", row.label(), words::setting(*value));
        } else if self.flag(row).is_some() {
            self.set_flag(row, step > 0);
        } else {
            self.cycle(row, step);
        }
    }

    /// Steps `row` through its choices.
    fn cycle(&mut self, row: Row, step: isize) {
        let next = |at: Option<usize>, length: usize| {
            (at.map_or(-1, |at| at as isize) + step).rem_euclid(length as isize) as usize
        };
        let fixed = self.fixed_protocol();
        let config = &mut self.config;
        self.notice = match row {
            Row::Advanced => {
                self.setup.advanced = !self.setup.advanced;
                return;
            }
            Row::Throughput => {
                let label = self.cycle_path(&THROUGHPUT, step);
                if self.fixed_protocol().is_some() {
                    self.config.paths.protocol = None;
                }
                format!("Throughput path: {label}.")
            }
            Row::Latency => format!("Latency path: {}.", self.cycle_path(&LATENCY, step)),
            Row::Protocol if fixed.is_some() => format!("This path serves {} only.", words::protocol(fixed)),
            Row::Protocol => {
                let versions = [None, Some(Protocol::Http1), Some(Protocol::Http2), Some(Protocol::Http3)];
                let at = versions.iter().position(|version| *version == config.paths.protocol);
                config.paths.protocol = versions[next(at, versions.len())];
                format!("HTTP version: {}.", words::protocol(config.paths.protocol))
            }
            Row::IdleCadence | Row::LoadedCadence => {
                let cadence = if row == Row::IdleCadence { &mut config.ping } else { &mut config.loaded_ping };
                let at = CADENCES.iter().position(|(preset, _)| preset == cadence);
                *cadence = CADENCES[next(at, CADENCES.len())].0;
                format!("{}: {}.", row.label(), words::cadence(*cadence))
            }
            Row::ForceStreams => {
                config.streams.forced = if config.streams.forced > 0 { 0 } else { config.streams.auto };
                streams_notice(config.streams, false)
            }
            Row::Streams => {
                let count = stream_count(config);
                *count = count.saturating_add_signed(step).clamp(1, MAX_STREAMS);
                streams_notice(config.streams, true)
            }
            _ => return,
        };
    }

    pub(super) fn edit(&mut self, row: Row, value: &str) {
        let mut editor = Editor { row, text: Vec::new(), cursor: 0, error: None };
        editor.insert(value);
        (self.overlay, self.notice) = (Overlay::Edit(editor), "Enter applies, esc cancels.".into());
    }

    /// A key while editing: enter applies, esc cancels, others edit the text.
    pub(super) fn edit_key(&mut self, key: Key) -> Vec<Effect> {
        let Overlay::Edit(editor) = &mut self.overlay else { return Vec::new() };
        match keys::find(keys::EDIT, key, |_| true) {
            Some(Action::Abort) => return vec![Effect::Quit],
            Some(Action::Cancel) => (self.overlay, self.notice) = (Overlay::None, "Edit canceled.".into()),
            Some(Action::Apply) => {
                let (row, text, before) = (editor.row, editor.text(), self.config.key());
                match self.parse(row, text.trim()) {
                    Ok(notice) => (self.overlay, self.notice) = (Overlay::None, notice),
                    Err(error) => {
                        if let Overlay::Edit(editor) = &mut self.overlay {
                            editor.error = Some(error.clone());
                        }
                        self.notice = error;
                    }
                }
                if self.config.key() != before {
                    self.recheck_soon();
                }
            }
            _ => editor.key(key),
        }
        Vec::new()
    }

    /// Applies `raw` to `row`: the notice it gives, or why it does not apply.
    fn parse(&mut self, row: Row, raw: &str) -> Result<String, String> {
        let config = &mut self.config;
        if row == Row::Catalogue {
            let url = server_origin(raw)?;
            if config.url.is_none() {
                // A first server is entered to be tested, so the next enter starts.
                self.setup.row = 0;
            } else if config.url.as_ref() != Some(&url) {
                config.servers.clear();
            }
            let notice = format!("Server {url}.");
            config.url = Some(url);
            return Ok(notice);
        }
        if row == Row::Streams {
            let count = raw.parse().ok().filter(|count| (1..=MAX_STREAMS).contains(count));
            *stream_count(config) = count.ok_or(format!("streams must be a whole number from 1 to {MAX_STREAMS}"))?;
            return Ok(streams_notice(config.streams, true));
        }
        let Some((value, bounds, range)) = span(config, row) else {
            return Ok(String::new());
        };
        let seconds = raw.parse::<f64>();
        let raw = seconds.map_or_else(|_| raw.to_owned(), |seconds| format!("{seconds}s"));
        let nanos =
            duration::parse(&raw).map_err(|_| "use a duration like 800ms, 4s, or 1m; a bare number is seconds")?;
        let parsed = u64::try_from(nanos).map_or(Duration::MAX, Duration::from_nanos);
        if !bounds.contains(&parsed) {
            return Err(format!("{} must be from {range}", row.label()));
        }
        *value = parsed;
        Ok(format!("{} {}.", row.label(), words::setting(parsed)))
    }
}

/// The duration `row` sets, the range it keeps to and that range in words.
fn span(config: &mut Config, row: Row) -> Option<(&mut Duration, RangeInclusive<Duration>, &'static str)> {
    match row {
        Row::Stage(stage) => Some((&mut config.durations[stage as usize], STAGE_LIMITS, "1s to 24h")),
        Row::Warmup => Some((&mut config.warmup, WARMUP, "0s to 4s")),
        _ => None,
    }
}

/// The forced stream count, or the automatic maximum.
fn stream_count(config: &mut Config) -> &mut usize {
    match config.streams.forced {
        0 => &mut config.streams.auto,
        _ => &mut config.streams.forced,
    }
}

/// What a stream change gives: forced counts, or automatic ones with the HTTP/1.1 maximum when `maximum`.
fn streams_notice(streams: Streams, maximum: bool) -> String {
    match (streams.forced, maximum) {
        (0, true) => format!("Stream count: Automatic · up to {} per direction.", streams.auto),
        (0, false) => "Stream count: Automatic.".into(),
        (forced, _) => format!("Stream count: Forced · {forced} per direction."),
    }
}

/// The inline editor: up to 4096 characters, a cursor, and the error its last apply found.
pub(super) struct Editor {
    pub row: Row,
    text: Vec<char>,
    cursor: usize,
    pub error: Option<String>,
}

impl Editor {
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// Inserts `text` at the cursor while room is left; tabs and line breaks become spaces.
    pub fn insert(&mut self, text: &str) {
        let spaced = text
            .chars()
            .map(|c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c });
        let room = MAX_TEXT.saturating_sub(self.text.len());
        let typed: Vec<_> = spaced.filter(|c| safe(*c)).take(room).collect();
        let at = self.cursor;
        self.cursor += typed.len();
        self.text.splice(at..at, typed);
        self.error = None;
    }

    fn key(&mut self, key: Key) {
        let length = self.text.len();
        match key {
            Key::Code(KeyCode::Left) => self.cursor = self.cursor.saturating_sub(1),
            Key::Code(KeyCode::Right) => self.cursor = (self.cursor + 1).min(length),
            Key::Code(KeyCode::Home) => self.cursor = 0,
            Key::Code(KeyCode::End) => self.cursor = length,
            Key::Code(KeyCode::Backspace) if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            Key::Code(KeyCode::Delete) if self.cursor < length => {
                self.text.remove(self.cursor);
            }
            Key::Code(KeyCode::Char(c)) => return self.insert(&c.to_string()),
            _ => return,
        }
        self.error = None;
    }

    /// The text around the cursor in `width` cells, the cursor a block over its character; `placeholder` greyed while
    /// the text is empty.
    pub fn line(&self, width: usize, palette: &Palette, placeholder: &str) -> Line {
        let cell = |c: &char| c.width().unwrap_or(0);
        if self.text.is_empty() && !placeholder.is_empty() {
            let mut rest = placeholder.chars();
            let under = rest.next().unwrap_or(' ').to_string();
            return Line::styled(under, palette.cursor)
                .and(rest.as_str(), palette.muted)
                .fit(width);
        }
        let under = self.text.get(self.cursor).copied().unwrap_or(' ');
        let mut room = width.max(1).saturating_sub(cell(&under));
        let mut start = self.cursor;
        while start > 0 && cell(&self.text[start - 1]) <= room {
            start -= 1;
            room -= cell(&self.text[start]);
        }
        let before: String = self.text[start..self.cursor].iter().collect();
        let after = self.text.iter().skip(self.cursor + 1).scan(room, |room, c| {
            *room = room.checked_sub(cell(c))?;
            Some(c)
        });
        Line::styled(before, palette.value)
            .and(under.to_string(), palette.cursor)
            .and(after.collect::<String>(), palette.value)
    }
}
