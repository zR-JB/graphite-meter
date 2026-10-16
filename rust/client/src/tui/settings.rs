//! What the settings sheet's keys change: steps, cycles, switches, a reset and the inline editor.
use super::{
    App, Effect, Overlay, RECHECK,
    keys::{self, Action, Key},
    paths::{LATENCY, THROUGHPUT},
    setup::Row,
};
use crate::{
    config::{Config, MAX_STREAMS, server_origin},
    report::vocabulary::CADENCES,
    text::Line,
    tui::theme::Palette,
};
use crossterm::event::KeyCode;
use graphite_meter_proto::{
    discovery::{Protocol, STAGE_LIMITS},
    duration,
    text::safe,
};
use std::{num::NonZero, ops::RangeInclusive, time::Duration};
use unicode_width::UnicodeWidthChar;

/// The most characters the editor holds.
const MAX_TEXT: usize = 4096;
const WARMUP: RangeInclusive<Duration> = Duration::ZERO..=Duration::from_secs(4);

impl App {
    /// Reacts to `action` on the focused row; a change to what a check depends on rechecks the paths.
    pub(super) fn settings_key(&mut self, action: Action, key: Key) -> Vec<Effect> {
        let (row, step) = (self.row(), key.step());
        let resetting = std::mem::take(&mut self.setup.resetting);
        let before = self.config.key();
        match action {
            Action::Move => self.move_focus(step),
            // Remembering writes or deletes a file, which the loop does.
            Action::Adjust if row == Row::Remember => return self.remember(step > 0),
            Action::Toggle | Action::Open if row == Row::Remember => return self.remember(!self.remembering),
            Action::Adjust => self.adjust(row, step),
            Action::Toggle => match self.flag(row) {
                Some(on) => self.set_flag(row, !on),
                None => self.activate(row, resetting),
            },
            Action::Open => self.activate(row, resetting),
            _ => {}
        }
        if self.config.key() != before {
            self.recheck_soon();
        }
        Vec::new()
    }

    /// A click on row `index`: the first focuses it, a second does what space does.
    pub(super) fn click_row(&mut self, index: usize) -> Vec<Effect> {
        if self.setup.row != index {
            (self.setup.row, self.setup.resetting) = (index, false);
            return Vec::new();
        }
        self.settings_key(Action::Toggle, Key::Code(KeyCode::Null))
    }

    /// Checks the paths once the settings rest; without a server there are none to check.
    pub(super) fn recheck_soon(&mut self) {
        self.recheck = self.config.url.as_ref().map(|_| self.now + RECHECK);
    }

    pub(super) fn set_flag(&mut self, row: Row, on: bool) {
        let before = self.config.key();
        match row {
            Row::Stage(stage) => {
                self.config.stages.retain(|kept| *kept != stage);
                self.config.stages.extend(on.then_some(stage));
                self.config.stages.sort_unstable();
            }
            Row::LoadedLatency => self.config.loaded_latency = on,
            Row::ForceStreams => self.config.streams.forced = NonZero::new(self.config.streams.auto).filter(|_| on),
            Row::Insecure => self.config.insecure = on,
            _ => {}
        }
        if self.config.key() != before {
            self.recheck_soon();
        }
    }

    /// Enter or space on `row`: open, edit, reset once confirmed, or step a choice.
    fn activate(&mut self, row: Row, confirmed: bool) {
        match row {
            Row::Servers => self.open_chooser(),
            Row::Reset if confirmed => {
                let (url, servers) = (self.config.url.clone(), std::mem::take(&mut self.config.servers));
                self.config = Config { url, servers, ..Config::default() };
                self.notify(Line::styled("Settings reset to their defaults.", self.palette.muted));
            }
            Row::Reset => self.setup.resetting = true,
            Row::Stage(_) | Row::Warmup => {
                let value = span(&mut self.config, row).map(|(value, ..)| duration::format(*value));
                self.edit(row, &value.unwrap_or_default());
            }
            Row::Catalogue => self.edit(row, &self.config.url.as_ref().map(ToString::to_string).unwrap_or_default()),
            Row::Streams => {
                let count = self.config.streams.shown().to_string();
                self.edit(row, &count);
            }
            _ => match self.flag(row) {
                Some(on) => self.set_flag(row, !on),
                None => self.cycle(row, 1),
            },
        }
    }

    /// ←/→ on `row`: a duration steps, a switch turns on or off, anything else cycles.
    fn adjust(&mut self, row: Row, step: isize) {
        if let Some((value, bounds, _)) = span(&mut self.config, row) {
            let unit = match value
                .saturating_sub(Duration::from_nanos(u64::from(step < 0)))
                .as_secs()
            {
                _ if row == Row::Warmup => Duration::from_millis(100),
                0..60 => Duration::from_secs(1),
                60..600 => Duration::from_secs(10),
                600..3600 => Duration::from_mins(1),
                _ => Duration::from_mins(5),
            };
            let moved = if step > 0 { value.saturating_add(unit) } else { value.saturating_sub(unit) };
            *value = moved.clamp(*bounds.start(), *bounds.end());
        } else if let Some(on) = self.flag(row) {
            // The stream count's switch flips either way, as a cycle does.
            self.set_flag(row, if row == Row::ForceStreams { !on } else { step > 0 });
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
        match row {
            Row::Throughput => {
                self.cycle_path(&THROUGHPUT, step);
                if self.fixed_protocol().is_some() {
                    self.config.paths.protocol = None;
                }
            }
            Row::Latency => _ = self.cycle_path(&LATENCY, step),
            Row::Protocol if fixed.is_none() => {
                let versions = [None, Some(Protocol::Http1), Some(Protocol::Http2), Some(Protocol::Http3)];
                let at = versions
                    .iter()
                    .position(|version| *version == self.config.paths.protocol);
                self.config.paths.protocol = versions[next(at, versions.len())];
            }
            Row::IdleCadence | Row::LoadedCadence => {
                let config = &mut self.config;
                let cadence = if row == Row::IdleCadence { &mut config.ping } else { &mut config.loaded_ping };
                let at = CADENCES.iter().position(|(preset, _)| preset == cadence);
                *cadence = CADENCES[next(at, CADENCES.len())].0;
            }
            Row::Streams => {
                let streams = &mut self.config.streams;
                let count = NonZero::new(streams.shown().saturating_add_signed(step).min(MAX_STREAMS));
                streams.show(count.unwrap_or(NonZero::<usize>::MIN));
            }
            _ => {}
        }
    }

    pub(super) fn edit(&mut self, row: Row, value: &str) {
        self.editor = Some(Editor::new(row, value));
    }

    /// A key while typing: enter applies, esc cancels, others edit the text.
    pub(super) fn edit_key(&mut self, key: Key) -> Vec<Effect> {
        if matches!(self.overlay, Overlay::Connect) && key == Key::Code(KeyCode::Tab) {
            return self.remember(!self.remembering);
        }
        let Some(editor) = &mut self.editor else { return Vec::new() };
        match keys::find(keys::EDIT, key, |_| true) {
            Some(Action::Abort) => return self.interrupt(),
            Some(Action::Cancel) => {
                self.editor = None;
                if matches!(self.overlay, Overlay::Connect) {
                    self.overlay = Overlay::None;
                }
            }
            Some(Action::Apply) => {
                let (row, text, before) = (editor.row, editor.text(), self.config.key());
                match self.parse(row, text.trim()) {
                    // The address being connected stays open while its check runs, and closes once it prepared.
                    Ok(()) if matches!(self.overlay, Overlay::Connect) => self.recheck = Some(self.now),
                    Ok(()) => self.editor = None,
                    Err(error) => {
                        if let Some(editor) = &mut self.editor {
                            editor.error = Some(error);
                        }
                    }
                }
                if self.config.key() != before && !matches!(self.overlay, Overlay::Connect) {
                    self.recheck_soon();
                }
            }
            _ => editor.key(key),
        }
        Vec::new()
    }

    /// Applies `raw` to `row`, or says why it does not apply.
    fn parse(&mut self, row: Row, raw: &str) -> Result<(), String> {
        let config = &mut self.config;
        if row == Row::Catalogue {
            let url = server_origin(raw)?;
            if config.url.as_ref() != Some(&url) {
                config.servers.clear();
            }
            config.url = Some(url);
            return Ok(());
        }
        if row == Row::Streams {
            let count = raw
                .parse::<NonZero<usize>>()
                .ok()
                .filter(|count| count.get() <= MAX_STREAMS);
            config
                .streams
                .show(count.ok_or(format!("Streams must be a whole number from 1 to {MAX_STREAMS}."))?);
            return Ok(());
        }
        let Some((value, bounds, range)) = span(config, row) else { return Ok(()) };
        let seconds = raw.parse::<f64>();
        let raw = seconds.map_or_else(|_| raw.to_owned(), |seconds| format!("{seconds}s"));
        let parsed = duration::parse(&raw).map_err(|_| "Use a time like 800ms, 4s or 1m; a bare number is seconds.")?;
        if !bounds.contains(&parsed) {
            return Err(format!("{} must be from {range}.", row.label()));
        }
        *value = parsed;
        Ok(())
    }
}

/// The duration `row` sets, the range it keeps to and that range in words.
fn span(config: &mut Config, row: Row) -> Option<(&mut Duration, RangeInclusive<Duration>, &'static str)> {
    match row {
        Row::Stage(stage) => Some((&mut config.durations[stage.index()], STAGE_LIMITS, "1 s to 24 h")),
        Row::Warmup => Some((&mut config.warmup, WARMUP, "0 s to 4 s")),
        _ => None,
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
    pub fn new(row: Row, text: &str) -> Self {
        let mut editor = Self { row, text: Vec::new(), cursor: 0, error: None };
        editor.insert(text);
        editor
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn clear(&mut self) {
        (self.text, self.cursor, self.error) = (Vec::new(), 0, None);
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
            Key::Code(KeyCode::Home) | Key::Ctrl('a') => self.cursor = 0,
            Key::Code(KeyCode::End) | Key::Ctrl('e') => self.cursor = length,
            Key::Code(KeyCode::Backspace) if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            Key::Code(KeyCode::Delete) if self.cursor < length => {
                self.text.remove(self.cursor);
            }
            Key::Ctrl('u') => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::Code(KeyCode::Char(c)) => return self.insert(&c.to_string()),
            _ => return,
        }
        self.error = None;
    }

    /// The text around the cursor in `width` cells, the cursor a block over its character.
    pub fn line(&self, width: usize, palette: &Palette) -> Line {
        let cell = |c: &char| c.width().unwrap_or(0);
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
