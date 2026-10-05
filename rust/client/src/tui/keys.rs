//! Key bindings: one table per screen, which both dispatch and the `?` help read.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A pressed key as bindings name it; shift only changes the character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Code(KeyCode),
    Ctrl(char),
}

impl Key {
    /// The key `event` presses; none with alt or another modifier held.
    pub fn of(event: KeyEvent) -> Option<Self> {
        match (event.code, event.modifiers - KeyModifiers::SHIFT) {
            (code, KeyModifiers::NONE) => Some(Self::Code(code)),
            (KeyCode::Char(c), KeyModifiers::CONTROL) => Some(Self::Ctrl(c.to_ascii_lowercase())),
            _ => None,
        }
    }

    /// The step the key moves by: back for up, left, k and shift+tab.
    pub fn step(self) -> isize {
        match self {
            Self::Code(KeyCode::Up | KeyCode::Left | KeyCode::BackTab | KeyCode::Char('k')) => -1,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Move,
    Adjust,
    Toggle,
    Open,
    Browse,
    Recheck,
    Servers,
    Available,
    Automatic,
    Select,
    Apply,
    Cancel,
    Cursor,
    Stop,
    Confirm,
    Setup,
    Again,
    Details,
    Close,
    Latency,
    Scroll,
    Page,
    Help,
    Quit,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub keys: &'static [Key],
    pub action: Action,
    /// The key and what it does as help names them; none keeps the binding out of help.
    pub help: Option<(&'static str, &'static str)>,
}

impl Binding {
    const fn new(keys: &'static [Key], action: Action, key: &'static str, does: &'static str) -> Self {
        Self { keys, action, help: Some((key, does)) }
    }

    /// The binding with what it does named `does`.
    pub const fn does(self, does: &'static str) -> Self {
        match self.help {
            Some((key, _)) => Self { help: Some((key, does)), ..self },
            None => self,
        }
    }

    const fn hidden(self) -> Self {
        Self { help: None, ..self }
    }
}

const fn code(code: KeyCode) -> Key {
    Key::Code(code)
}

const fn char(c: char) -> Key {
    Key::Code(KeyCode::Char(c))
}

const UP: Key = code(KeyCode::Up);
const DOWN: Key = code(KeyCode::Down);
const LEFT: Key = code(KeyCode::Left);
const RIGHT: Key = code(KeyCode::Right);
const HOME: Key = code(KeyCode::Home);
const END: Key = code(KeyCode::End);
const ENTER: Key = code(KeyCode::Enter);
const ESC: Key = code(KeyCode::Esc);
const SPACE: Key = char(' ');

pub const START: Binding = Binding::new(&[char('r')], Action::Start, "r", "start test");
pub const MOVE: Binding = Binding::new(
    &[UP, DOWN, char('k'), char('j'), code(KeyCode::Tab), code(KeyCode::BackTab)],
    Action::Move,
    "↑/↓",
    "move",
);
pub const ADJUST: Binding = Binding::new(&[LEFT, RIGHT], Action::Adjust, "←/→", "change");
pub const TOGGLE: Binding = Binding::new(&[SPACE], Action::Toggle, "space", "on/off");
pub const OPEN: Binding = Binding::new(&[ENTER, SPACE], Action::Open, "enter", "open");
const RECHECK: Binding = Binding::new(&[char('v')], Action::Recheck, "v", "recheck paths");
const SERVERS: Binding = Binding::new(&[char('s')], Action::Servers, "s", "test servers");
const AVAILABLE: Binding = Binding::new(&[char('u')], Action::Available, "u", "use available servers");
const AUTOMATIC: Binding = Binding::new(&[char('a')], Action::Automatic, "a", "automatic paths");
const SELECT: Binding = Binding::new(&[SPACE], Action::Select, "space", "select");
const APPLY: Binding = Binding::new(&[ENTER], Action::Apply, "enter", "apply");
const CANCEL: Binding = Binding::new(&[ESC], Action::Cancel, "esc", "cancel");
const BROWSE: Binding = Binding::new(&[ENTER, SPACE, char('o')], Action::Browse, "enter/space", "open page");
const CURSOR: Binding = Binding::new(&[LEFT, RIGHT, HOME, END], Action::Cursor, "←/→", "move");
const STOP: Binding = Binding::new(&[ESC], Action::Stop, "esc", "stop test");
const CONFIRM_STOP: Binding = Binding::new(&[ESC], Action::Confirm, "esc", "confirm stop");
const SETUP_AGAIN: Binding = Binding::new(&[ESC], Action::Setup, "esc", "setup");
const AGAIN: Binding = Binding::new(&[ENTER, char('r')], Action::Again, "enter", "run again");
const DETAILS_OPEN: Binding = Binding::new(&[char('d')], Action::Details, "d", "details");
const CLOSE: Binding = Binding::new(&[ESC, char('d')], Action::Close, "esc", "close");
const LATENCY: Binding = Binding::new(&[char('l')], Action::Latency, "l", "latency server");
const SCROLL: Binding = Binding::new(&[UP, DOWN, char('k'), char('j')], Action::Scroll, "↑/↓", "scroll");
pub const PAGE: Binding =
    Binding::new(&[code(KeyCode::PageUp), code(KeyCode::PageDown), HOME, END], Action::Page, "pgdn", "more");
pub const HELP: Binding = Binding::new(&[char('?')], Action::Help, "?", "keys");
pub const QUIT: Binding = Binding::new(&[char('q')], Action::Quit, "q", "quit");
const ABORT: Binding = Binding::new(&[Key::Ctrl('c')], Action::Abort, "ctrl+c", "quit");

#[rustfmt::skip]
pub const SETUP: &[Binding] = &[
    START, MOVE, ADJUST, TOGGLE, OPEN.does("start or open"), RECHECK, SERVERS, AVAILABLE, AUTOMATIC, PAGE, HELP, QUIT,
    ABORT.hidden(),
];
pub const EDIT: &[Binding] = &[CURSOR, APPLY, CANCEL, ABORT];
pub const CHOOSER: &[Binding] = &[MOVE, SELECT, APPLY, CANCEL, QUIT, HELP.hidden(), ABORT.hidden()];
pub const SIGN_IN: &[Binding] = &[BROWSE, CANCEL, QUIT, PAGE.hidden(), HELP.hidden(), ABORT.hidden()];
#[rustfmt::skip]
pub const RUN: &[Binding] = &[
    STOP, AGAIN, SETUP_AGAIN, DETAILS_OPEN, LATENCY, SCROLL.hidden(), PAGE.hidden(), HELP, QUIT, ABORT.hidden(),
];
pub const DETAILS: &[Binding] = &[SCROLL, CLOSE, QUIT, PAGE.hidden(), HELP.hidden(), ABORT.hidden()];
pub const CONFIRM: &[Binding] = &[CONFIRM_STOP, QUIT, ABORT.hidden()];

/// The first binding of `table` for `key` whose action is `offered`.
pub fn find(table: &[Binding], key: Key, offered: impl Fn(Action) -> bool) -> Option<Action> {
    let bindings = table.iter().filter(|binding| binding.keys.contains(&key));
    bindings.map(|binding| binding.action).find(|&action| offered(action))
}

/// The bindings of `table` that help lists while their actions are `offered`.
pub fn listed(table: &[Binding], offered: impl Fn(Action) -> bool) -> Vec<Binding> {
    let listed = |binding: &&Binding| binding.help.is_some() && offered(binding.action);
    table.iter().filter(listed).copied().collect()
}
