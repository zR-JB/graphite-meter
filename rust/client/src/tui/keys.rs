//! Binds keys in one table per context, which dispatch, the bottom bar and the keys sheet all read.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Names a pressed key as bindings do. Shift only changes the character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Code(KeyCode),
    Ctrl(char),
}

impl Key {
    /// Reads the key `event` presses, none with alt or another modifier held.
    pub fn of(event: KeyEvent) -> Option<Self> {
        match (event.code, event.modifiers - KeyModifiers::SHIFT) {
            (code, KeyModifiers::NONE) => Some(Self::Code(code)),
            (KeyCode::Char(c), KeyModifiers::CONTROL) => Some(Self::Ctrl(c.to_ascii_lowercase())),
            _ => None,
        }
    }

    /// The key moves by this step, back for up, left, k and shift+tab.
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
    Stop,
    Settings,
    Details,
    Help,
    /// 1 to 4 switch a stage on or off.
    Stage,
    Latency,
    Recheck,
    Move,
    Adjust,
    Toggle,
    Open,
    Close,
    Scroll,
    Page,
    Select,
    Apply,
    Cancel,
    Cursor,
    Browse,
    Quit,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub keys: &'static [Key],
    pub action: Action,
    /// Names the key and what it does as the bar and the keys sheet show them. None keeps the binding out of both.
    pub help: Option<(&'static str, &'static str)>,
}

impl Binding {
    const fn new(
        keys: &'static [Key],
        action: Action,
        key: &'static str,
        does: &'static str,
    ) -> Self {
        Self {
            keys,
            action,
            help: Some((key, does)),
        }
    }

    /// Renames what the binding does to `does`.
    pub const fn does(self, does: &'static str) -> Self {
        let Some((key, _)) = self.help else {
            return self;
        };
        Self {
            help: Some((key, does)),
            ..self
        }
    }

    const fn hidden(self) -> Self {
        Self { help: None, ..self }
    }

    const fn on(self, keys: &'static [Key], key: &'static str) -> Self {
        let Some((_, does)) = self.help else {
            return Self { keys, ..self };
        };
        Self {
            keys,
            help: Some((key, does)),
            ..self
        }
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

pub const START: Binding = Binding::new(
    &[ENTER, SPACE, char('r')],
    Action::Start,
    "enter",
    "start test",
);
pub const STOP: Binding = Binding::new(&[ESC], Action::Stop, "esc", "stop test");
pub const SETTINGS: Binding = Binding::new(&[char('s')], Action::Settings, "s", "settings");
pub const DETAILS: Binding = Binding::new(&[char('d')], Action::Details, "d", "details");
pub const HELP: Binding = Binding::new(&[char('?')], Action::Help, "?", "keys");
const STAGES: Binding = Binding::new(
    &[char('1'), char('2'), char('3'), char('4')],
    Action::Stage,
    "1 to 4",
    "stages on/off",
);
const LATENCY: Binding = Binding::new(&[char('l')], Action::Latency, "l", "latency server");
const RECHECK: Binding = Binding::new(&[char('v')], Action::Recheck, "v", "check the server again");
const MOVE: Binding = Binding::new(
    &[
        UP,
        DOWN,
        char('k'),
        char('j'),
        code(KeyCode::Tab),
        code(KeyCode::BackTab),
    ],
    Action::Move,
    "↑↓",
    "move",
);
const ADJUST: Binding = Binding::new(&[LEFT, RIGHT], Action::Adjust, "←→", "change");
const TOGGLE: Binding = Binding::new(&[SPACE], Action::Toggle, "space", "on/off");
const OPEN: Binding = Binding::new(&[ENTER], Action::Open, "enter", "edit");
pub const CLOSE: Binding = Binding::new(&[ESC], Action::Close, "esc", "close");
const SCROLL: Binding = Binding::new(
    &[UP, DOWN, char('k'), char('j')],
    Action::Scroll,
    "↑↓",
    "scroll",
);
const PAGE: Binding = Binding::new(
    &[code(KeyCode::PageUp), code(KeyCode::PageDown), HOME, END],
    Action::Page,
    "pgdn",
    "page",
);
const SELECT: Binding = Binding::new(&[SPACE], Action::Select, "space", "select");
const APPLY: Binding = Binding::new(&[ENTER], Action::Apply, "enter", "apply");
const CANCEL: Binding = Binding::new(&[ESC], Action::Cancel, "esc", "cancel");
const CURSOR: Binding = Binding::new(&[LEFT, RIGHT, HOME, END], Action::Cursor, "←→", "move");
const BROWSE: Binding = Binding::new(
    &[ENTER, char('o')],
    Action::Browse,
    "enter",
    "open the sign-in page",
);
pub const QUIT: Binding = Binding::new(&[char('q')], Action::Quit, "q", "quit");
const ABORT: Binding = Binding::new(&[Key::Ctrl('c')], Action::Abort, "ctrl+c", "quit at once");

pub const CONSOLE: &[Binding] = &[
    START,
    STOP,
    SETTINGS,
    DETAILS,
    STAGES,
    LATENCY,
    RECHECK,
    HELP,
    QUIT,
    ABORT.hidden(),
];
#[rustfmt::skip]
pub const SETTINGS_SHEET: &[Binding] = &[
    MOVE, ADJUST, TOGGLE, OPEN, START.on(&[char('r')], "r"), CLOSE, CLOSE.on(&[char('s')], "s").hidden(), HELP, QUIT,
    ABORT.hidden(),
];
#[rustfmt::skip]
pub const DETAILS_SHEET: &[Binding] = &[
    SCROLL, PAGE, CLOSE, CLOSE.on(&[char('d')], "d").hidden(), HELP, QUIT, ABORT.hidden(),
];
pub const HELP_SHEET: &[Binding] = &[
    SCROLL,
    CLOSE,
    CLOSE.on(&[char('?')], "?").hidden(),
    QUIT,
    ABORT.hidden(),
];
pub const CHOOSER: &[Binding] = &[MOVE, SELECT, APPLY, CANCEL, QUIT, ABORT.hidden()];
pub const SIGN_IN: &[Binding] = &[BROWSE, CANCEL.does("cancel sign-in"), QUIT, ABORT.hidden()];
pub const EDIT: &[Binding] = &[CURSOR, APPLY, CANCEL, ABORT.hidden()];

/// Finds the first binding of `table` for `key` whose action is `offered`.
pub fn find(table: &[Binding], key: Key, offered: impl Fn(Action) -> bool) -> Option<Action> {
    let bindings = table.iter().filter(|binding| binding.keys.contains(&key));
    bindings
        .map(|binding| binding.action)
        .find(|&action| offered(action))
}

/// Lists the bindings of `table` that help shows while their actions are `offered`.
pub fn listed(table: &[Binding], offered: impl Fn(Action) -> bool) -> Vec<Binding> {
    let listed = |binding: &&Binding| binding.help.is_some() && offered(binding.action);
    table.iter().filter(listed).copied().collect()
}
