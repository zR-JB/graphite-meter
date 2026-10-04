//! Go's keymap (keys.go): each binding's keys and its help, and the hints each mode offers.
use super::{Popup, Ui, setup::Setting};
use crate::report::{pad, span};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// A key.Binding: the key names it matches, and its help key and description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    keys: &'static [&'static str],
    pub key: &'static str,
    pub desc: &'static str,
}

const fn bind(keys: &'static [&'static str], key: &'static str, desc: &'static str) -> Binding {
    Binding { keys, key, desc }
}

pub(super) const ROWS: Binding = bind(&["up", "down", "k", "j", "tab", "shift+tab"], "↑/↓", "move");
pub(super) const ADJUST: Binding = bind(&["left", "right"], "←/→", "change");
pub(super) const CHANGE: Binding = bind(&["enter", "space"], "enter", "open");
pub(super) const TOGGLE: Binding = bind(&["space"], "space", "on/off");
pub(super) const START: Binding = bind(&["r"], "r", "start test");
pub(super) const RECHECK: Binding = bind(&["v"], "v", "recheck paths");
pub(super) const SERVERS: Binding = bind(&["s"], "s", "test servers");
pub(super) const AUTOMATIC: Binding = bind(&["a"], "a", "automatic paths");
pub(super) const AVAILABLE: Binding = bind(&["u"], "u", "use available servers");
pub(super) const OPEN_SIGN_IN: Binding = bind(&["enter", "space", "o"], "enter/space", "open page");
pub(super) const CANCEL_SIGN_IN: Binding = bind(&["esc"], "esc", "cancel");
pub(super) const STOP: Binding = bind(&["esc"], "esc", "stop test");
pub(super) const CONFIRM_STOP: Binding = bind(&["esc"], "esc", "confirm stop");
pub(super) const SETUP: Binding = bind(&["esc"], "esc", "setup");
pub(super) const RUN_AGAIN: Binding = bind(&["enter", "r"], "enter", "run again");
pub(super) const LATENCY_SERVER: Binding = bind(&["l"], "l", "latency server");
pub(super) const DETAILS: Binding = bind(&["d"], "d", "details");
pub(super) const SCROLL: Binding = bind(&["up", "down", "k", "j"], "↑/↓", "scroll");
pub(super) const PAGE: Binding = bind(&["pgup", "pgdown", "home", "end"], "pgdn", "more");
pub(super) const CLOSE: Binding = bind(&["esc"], "esc", "close");
pub(super) const TOGGLE_SERVER: Binding = bind(&["space"], "space", "select");
pub(super) const APPLY: Binding = bind(&["enter"], "enter", "apply");
pub(super) const DISCARD: Binding = bind(&["esc"], "esc", "cancel");
const CURSOR: Binding = bind(&["left", "right", "home", "end"], "←/→", "move");
pub(super) const HELP: Binding = bind(&["?"], "?", "keys");
pub(super) const QUIT: Binding = bind(&["q"], "q", "quit");
pub(super) const ABORT: Binding = bind(&["ctrl+c"], "ctrl+c", "quit");

impl Binding {
    pub(super) fn matches(self, name: &str) -> bool {
        self.keys.contains(&name)
    }

    /// Go's hint: the binding under another description.
    const fn hint(self, desc: &'static str) -> Self {
        Self { desc, ..self }
    }
}

/// The keys Bubble Tea names by a word.
#[rustfmt::skip]
pub(super) const NAMED: [(KeyCode, &str); 15] = [
    (KeyCode::Up, "up"), (KeyCode::Down, "down"), (KeyCode::Left, "left"), (KeyCode::Right, "right"),
    (KeyCode::Enter, "enter"), (KeyCode::Esc, "esc"), (KeyCode::Tab, "tab"), (KeyCode::BackTab, "shift+tab"),
    (KeyCode::Backspace, "backspace"), (KeyCode::Delete, "delete"), (KeyCode::Home, "home"), (KeyCode::End, "end"),
    (KeyCode::PageUp, "pgup"), (KeyCode::PageDown, "pgdown"), (KeyCode::Char(' '), "space"),
];

/// A press as Bubble Tea names it, such as "down", "shift+tab", "ctrl+c" or "q".
pub(super) fn name(key: KeyEvent) -> String {
    let base = match (NAMED.iter().find(|(code, _)| *code == key.code), key.code) {
        (Some((KeyCode::BackTab, name)), _) => return (*name).into(),
        (Some((_, name)), _) => (*name).into(),
        (None, KeyCode::Char(character)) => character.to_lowercase().collect::<String>(),
        _ => return String::new(),
    };
    let mut name = String::new();
    for (held, prefix) in [(KeyModifiers::CONTROL, "ctrl+"), (KeyModifiers::ALT, "alt+")] {
        name.extend(key.modifiers.contains(held).then_some(prefix));
    }
    // Bubble Tea names a shifted letter by its capital, which no binding matches.
    if key.code.as_char().is_some_and(char::is_uppercase) {
        name.push_str("shift+");
    }
    name + &base
}

/// Go's reverse: the keys that move up, back or left.
pub(super) fn reverse(name: &str) -> bool {
    matches!(name, "shift+tab" | "left" | "up" | "k")
}

/// Go's delta.
pub(super) fn delta(name: &str) -> isize {
    if reverse(name) { -1 } else { 1 }
}

impl Ui {
    /// Go's ShortHelp: the footer's bindings in this state.
    pub(super) fn short_help(&self) -> Vec<Binding> {
        match self.popup {
            Popup::Details => return vec![SCROLL, CLOSE, QUIT],
            Popup::Servers => return vec![ROWS, TOGGLE_SERVER, APPLY, DISCARD, QUIT],
            Popup::None => {}
        }
        if self.edit.is_some() {
            return vec![CURSOR, APPLY, DISCARD, ABORT];
        }
        if self.stop_prompt {
            return vec![CONFIRM_STOP, QUIT];
        }
        if self.shown().is_some() || self.running() {
            let mut bindings = match self.running() {
                true => vec![STOP, DETAILS],
                false => vec![RUN_AGAIN, SETUP, DETAILS],
            };
            bindings.extend(self.several().then_some(LATENCY_SERVER));
            return [bindings, vec![HELP, QUIT]].concat();
        }
        if self.snapshot.auth.is_some() {
            return vec![OPEN_SIGN_IN, CANCEL_SIGN_IN, QUIT];
        }
        let row = self.current();
        if row == Setting::Start {
            return vec![CHANGE.hint("start test"), ROWS, HELP, QUIT];
        }
        let mut bindings = vec![START, ROWS];
        bindings.extend(row.adjusts().then_some(ADJUST));
        bindings.extend(matches!(row, Setting::Stage(_)).then_some(TOGGLE));
        bindings.extend([CHANGE.hint(row.enter_verb()), HELP, QUIT]);
        bindings
    }

    /// Go's FullHelp: setup's every key, or the short help, in columns of three.
    pub(super) fn full_help(&self) -> Vec<Binding> {
        if self.shown().is_some() || self.snapshot.auth.is_some() || self.edit.is_some() || self.popup != Popup::None {
            return self.short_help();
        }
        let mut all = vec![START, ROWS, ADJUST, TOGGLE, CHANGE.hint("start or open"), RECHECK];
        all.extend(self.can_choose_servers().then_some(SERVERS));
        all.extend(self.can_use_available().then_some(AVAILABLE));
        all.extend([AUTOMATIC, PAGE, HELP, QUIT]);
        all
    }

    /// help.ShortHelpView: "key desc" items between separators.
    pub(super) fn short_view(&self, bindings: &[Binding]) -> Line<'static> {
        let mut spans = Vec::new();
        for binding in bindings {
            if !spans.is_empty() {
                spans.push(span(" • ", self.theme.border));
            }
            spans.extend([span(binding.key, self.theme.text), Span::raw(" "), span(binding.desc, self.theme.muted)]);
        }
        Line::from(spans)
    }

    /// help.FullHelpView: columns of three, keys beside their descriptions, four spaces apart.
    pub(super) fn full_view(&self, bindings: &[Binding]) -> Vec<Line<'static>> {
        let mut lines = vec![Line::default(); bindings.len().min(3)];
        let cell = |text, style, width| pad(Line::from(span(text, style)), width).spans;
        for (index, column) in bindings.chunks(3).enumerate() {
            let key_width = column.iter().map(|binding| binding.key.width()).max().unwrap_or(0);
            let desc_width = column.iter().map(|binding| binding.desc.width()).max().unwrap_or(0);
            for (row, line) in lines.iter_mut().enumerate() {
                if index > 0 {
                    line.spans.push(span("    ", self.theme.border));
                }
                let (key, desc) = column.get(row).map_or(("", ""), |binding| (binding.key, binding.desc));
                line.spans.extend(cell(key, self.theme.text, key_width));
                line.spans.push(Span::raw(" "));
                line.spans.extend(cell(desc, self.theme.muted, desc_width));
            }
        }
        lines
    }
}
