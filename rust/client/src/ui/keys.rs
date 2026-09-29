//! Go's keymap: each binding's keys, the hint footers show for it and its help grid entry, so
//! dispatch, the footers and the help grid cannot disagree.
use super::{InputMode, Ui, run_servers};
use crossterm::event::KeyCode::{
    self, BackTab, Char, Down, End, Enter, Esc, Home, Left, PageDown, PageUp, Right, Tab, Up,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    codes: &'static [KeyCode],
    pub(super) hint: &'static str,
    help: &'static str,
}

const fn key(codes: &'static [KeyCode], hint: &'static str, help: &'static str) -> Key {
    Key { codes, hint, help }
}

pub(super) const ROWS: Key = key(
    &[Up, Down, Char('k'), Char('j'), Tab, BackTab],
    "↓ settings",
    "Tab/Shift-Tab focus",
);
pub(super) const CHANGE: Key = key(&[Left, Right], "", "arrows change");
pub(super) const ACTIVATE: Key = key(&[Enter], "", "Enter edit/run");
pub(super) const TOGGLE: Key = key(&[Char(' ')], "Space toggle", "Space stage");
pub(super) const STOP: Key = key(&[Esc], "Esc stop", "Esc close/stop");
pub(super) const START: Key = key(&[Char('r')], "r Start test", "r run");
pub(super) const RECHECK: Key = key(&[Char('v')], "v Recheck paths", "v recheck");
pub(super) const SERVERS: Key = key(&[Char('s')], "s servers", "s servers");
pub(super) const AVAILABLE: Key = key(&[Char('u')], "", "u available");
pub(super) const AUTOMATIC: Key = key(&[Char('a')], "", "a auto");
pub(super) const DETAILS: Key = key(&[Char('d')], "d Details", "d Details");
pub(super) const LATENCY: Key = key(&[Char('l')], "l Latency server", "l latency");
pub(super) const OPEN: Key = key(&[Char('o'), Enter, Char(' ')], "Enter/Space/o open", "o sign-in");
pub(super) const QUIT: Key = key(&[Char('q')], "q quit", "q quit");
/// Ctrl-C, which dispatch reads with its modifier.
const ABORT: Key = key(&[], "", "Ctrl-C stop");
pub(super) const HELP: Key = key(&[Char('?')], "? keys", "? keys");
pub(super) const CANCEL: Key = key(&[Esc], "Esc cancel", "");
pub(super) const CONFIRM_STOP: Key = key(&[Esc], "Esc confirm stop", "");
/// Any key that does not confirm the stop.
const CONTINUE: Key = key(&[], "any key continue", "");
pub(super) const SETUP: Key = key(&[Esc], "Esc setup", "");
pub(super) const RUN_AGAIN: Key = key(&[Enter, Char('r')], "Enter Run again", "");
const BEGIN: Key = key(&[Enter], "Enter Start test", "");
pub(super) const MORE: Key = key(&[PageUp, PageDown, Home, End], "PgDn more", "");
pub(super) const SCROLL: Key = key(&[Up, Down, Char('k'), Char('j')], "↑/↓ scroll", "");
pub(super) const CLOSE: Key = key(&[Char('d'), Esc], "d/Esc close", "");
pub(super) const APPLY: Key = key(&[Enter], "Enter apply", "");
pub(super) const DISCARD: Key = key(&[Esc], "Esc discard", "");
const CURSOR: Key = key(&[Left, Right, Home, End], "←/→ Home/End move", "");

/// The sign-in popup's keys, which its footer lists.
pub(super) const SIGN_IN: [Key; 3] = [OPEN, CANCEL, QUIT];
/// The editor's keys, which its popup lists.
pub(super) const EDIT: [Key; 3] = [APPLY, DISCARD, CURSOR];
/// Go's FullHelp: the help grid, row by row.
const FULL_HELP: [&[Key]; 6] = [
    &[ROWS, CHANGE],
    &[ACTIVATE, TOGGLE],
    &[STOP, START, RECHECK],
    &[SERVERS, AVAILABLE, AUTOMATIC],
    &[DETAILS, LATENCY, OPEN],
    &[QUIT, ABORT, HELP],
];

impl Key {
    pub(super) fn matches(self, code: KeyCode) -> bool {
        self.codes.contains(&code)
    }
}

/// The first of `keys` that `code` presses.
pub(super) fn pressed(code: KeyCode, keys: &[Key]) -> Option<Key> {
    keys.iter().copied().find(|key| key.matches(code))
}

pub(super) fn hints(keys: &[Key]) -> Vec<&'static str> {
    keys.iter().map(|key| key.hint).collect()
}

pub(super) fn full_help() -> impl Iterator<Item = Vec<&'static str>> {
    FULL_HELP.iter().map(|row| row.iter().map(|key| key.help).collect())
}

impl Ui {
    /// Go's ShortHelp: the footer's hints in this state. The popups and the editor keep those
    /// of the view under them.
    pub(super) fn short_help(&self) -> Vec<&'static str> {
        match self.mode() {
            InputMode::Confirm => hints(&[CONFIRM_STOP, CONTINUE, QUIT]),
            _ if self.live => {
                let run: &[Key] = if self.active() { &[STOP] } else { &[RUN_AGAIN, SETUP] };
                let latency = (run_servers(&self.snapshot).len() > 1).then_some(LATENCY);
                hints(&[run, &[DETAILS], latency.as_slice(), &[HELP, QUIT]].concat())
            }
            _ if self.rows.selected() == Some(0) => hints(&[BEGIN, ROWS, SERVERS, RECHECK, HELP, QUIT]),
            _ => [self.field().hints(), &hints(&[START, HELP, QUIT])].concat(),
        }
    }
}
