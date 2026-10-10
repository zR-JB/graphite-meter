//! The terminal interface: the `App` whose screens the event loop in `terminal` draws.
pub mod chrome;
mod console;
mod dialogs;
mod frame;
mod keys;
mod paths;
mod run;
mod settings;
mod setup;
mod terminal;
pub mod theme;

pub use terminal::interactive;

use crate::{
    INTERRUPTED,
    config::{Config, PrepKey},
    controller::Command,
    events::{Event, Run, SignInEnd, View},
    model::Outcome,
    net::approval,
    report,
    text::Profile,
};
use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use dialogs::{Chooser, SignIn};
use graphite_meter_proto::origin::Origin;
use keys::{Action, Key};
use ratatui_core::layout::Rect;
use settings::Editor;
use setup::{Row, Setup};
use std::time::{Duration, Instant};
use theme::{Answer, Palette, Taken};

/// How long settings rest before their paths are checked again.
const RECHECK: Duration = Duration::from_millis(350);
/// How long a check's paths serve a run.
const FRESH: Duration = Duration::from_secs(30);
const FRAME: Duration = Duration::from_millis(33);
/// The smallest terminal the interface draws in, in columns and rows.
const SMALLEST: (usize, usize) = (40, 12);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

enum Screen {
    Setup,
    Chooser(Chooser),
    SignIn(SignIn),
    Run,
}

enum Overlay {
    None,
    Edit(Editor),
    Details,
    ConfirmStop,
}

/// What the interface asks of its loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Command(Box<Command>),
    /// Opens the sign-in page at this address in a browser.
    OpenBrowser(String),
    /// Keeps this server for the next start.
    Remember(Origin),
    Quit,
}

impl Effect {
    fn command(command: Command) -> Vec<Self> {
        vec![Self::Command(Box::new(command))]
    }
}

/// How the interface ended: what it showed, its palette, an interrupt's status, whether a finished report follows.
pub struct Exit {
    pub view: View,
    pub palette: Palette,
    pub signal: Option<u8>,
    pub report: bool,
}

/// The interface's state; time enters through each call.
pub struct App {
    config: Config,
    view: View,
    profile: Profile,
    palette: Palette,
    /// The terminal's background answer while it arrives.
    answer: Answer,
    screen: Screen,
    overlay: Overlay,
    setup: Setup,
    live: run::Live,
    help: bool,
    notice: String,
    /// Whether the interface quits once the run stops.
    quitting: bool,
    /// Whether ctrl+c or a signal arrived during a run.
    interrupted: bool,
    /// The status of the latest caught signal.
    caught: Option<u8>,
    /// When the paths are checked next.
    recheck: Option<Instant>,
    /// When the checked paths turn stale, until a frame shows it.
    stale: Option<Instant>,
    /// What the latest operation prepared for, and when its paths arrived.
    asked: Option<PrepKey>,
    checked: Option<(PrepKey, Instant)>,
    /// The server last kept for the next start.
    kept: Option<Origin>,
    /// The body's first shown line, whether it follows the focused row, and the lines it shows.
    scroll: usize,
    follow: bool,
    rows: usize,
    /// The last frame's size.
    area: Rect,
    since: Instant,
    now: Instant,
}

impl App {
    /// Setup for `config` in `profile`; its first tick checks the paths, or it asks for the server without one.
    pub fn new(config: Config, profile: Profile, now: Instant) -> Self {
        let mut app = Self {
            config,
            view: View::default(),
            profile,
            palette: Palette::new(true),
            answer: Answer::default(),
            screen: Screen::Setup,
            overlay: Overlay::None,
            setup: Setup::default(),
            live: run::Live::default(),
            help: false,
            notice: String::new(),
            quitting: false,
            interrupted: false,
            caught: None,
            recheck: None,
            stale: None,
            asked: None,
            checked: None,
            kept: None,
            scroll: 0,
            follow: false,
            rows: 0,
            area: Rect::default(),
            since: now,
            now,
        };
        match app.config.url.clone() {
            Some(url) => (app.recheck, app.kept) = (Some(now), Some(url)),
            None => app.ask_for_server(),
        }
        app
    }

    /// Opens the server's address for typing: nothing can be checked or tested without one.
    fn ask_for_server(&mut self) {
        self.screen = Screen::Setup;
        self.setup.row = self.rows().iter().position(|row| *row == Row::Catalogue).unwrap_or(0);
        self.edit(Row::Catalogue, "");
        self.notice = "Enter your Graphite Meter server's address, then press enter.".into();
    }

    /// Takes `event`; a run ending while the interface quits ends it.
    pub fn event(&mut self, event: &Event, now: Instant) -> Vec<Effect> {
        self.now = now;
        let shown = self.latency_server().cloned();
        self.view.apply(event);
        self.live.event(event, self.view.run.as_ref(), shown.as_ref(), now);
        match event {
            Event::Prepared { servers, .. } => {
                (self.checked, self.stale) = (self.asked.clone().map(|key| (key, now)), Some(now + FRESH));
                if std::mem::take(&mut self.setup.chooser) {
                    self.open_chooser();
                }
                let prepared = !servers.is_empty() && servers.iter().all(|server| server.path.is_ok());
                if prepared && self.config.url != self.kept {
                    self.kept.clone_from(&self.config.url);
                    return self.kept.clone().map(Effect::Remember).into_iter().collect();
                }
            }
            Event::CheckFailed(_) => self.setup.chooser = false,
            Event::SignIn(_) => {
                (self.screen, self.overlay) = (Screen::SignIn(SignIn { since: now, opened: false }), Overlay::None);
                self.notice = "Check the code, then press enter to open the sign-in page.".into();
            }
            Event::SignInEnded(end) => {
                if matches!(self.screen, Screen::SignIn(_)) {
                    self.screen = if self.running() { Screen::Run } else { Screen::Setup };
                }
                match end {
                    SignInEnd::Approved => self.notice = "Signed in. Checking the authenticated paths…".into(),
                    SignInEnd::Expired => self.notice = approval::EXPIRED.into(),
                    SignInEnd::Failed | SignInEnd::Cancelled => {}
                }
            }
            Event::RunStarted { .. } => self.notice = "Test started. Press esc to stop.".into(),
            Event::ServerFailed(failure) => {
                let name = self.view.servers.iter().find(|path| path.id == failure.server);
                let name = name.map_or(failure.server.as_str(), |path| path.name.as_str());
                self.notice = format!("{name}: {}", failure.failure.reason.label());
            }
            Event::RunFinished { error, .. } => {
                if matches!(self.overlay, Overlay::ConfirmStop) {
                    self.overlay = Overlay::None;
                }
                self.notice.clear();
                if let Some(reason) = report::unreported(&self.view) {
                    (self.screen, self.notice) = (Screen::Setup, reason);
                    self.recheck = error.is_some().then(|| now + RECHECK);
                }
                if self.quitting {
                    return vec![Effect::Quit];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Reacts to terminal input: keys, pasted text, the mouse wheel and clicks on the key.
    pub fn input(&mut self, input: Input, now: Instant) -> Vec<Effect> {
        self.now = now;
        match input {
            Input::Key(key) if key.kind != KeyEventKind::Release => return self.key(key, now),
            Input::Paste(text) => {
                if let Overlay::Edit(editor) = &mut self.overlay {
                    editor.insert(text.trim());
                }
            }
            Input::Mouse(mouse) if self.wheels() => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(3),
                // A key does what its cap says: Stop while running, otherwise start or run again.
                MouseEventKind::Down(MouseButton::Left)
                    if matches!((&self.screen, &self.overlay), (Screen::Setup | Screen::Run, Overlay::None))
                        && self.on_key(mouse.column, mouse.row) =>
                {
                    let cap = if self.running() { KeyCode::Esc } else { KeyCode::Char('r') };
                    return self.press(Key::Code(cap));
                }
                _ => {}
            },
            _ => {}
        }
        Vec::new()
    }

    /// Reacts to a key, or takes it as part of the terminal's background answer.
    pub fn key(&mut self, event: KeyEvent, now: Instant) -> Vec<Effect> {
        self.now = now;
        match self.answer.take(event) {
            Taken::Not => {}
            Taken::Part => return Vec::new(),
            Taken::Background(dark) => {
                self.palette = Palette::new(dark);
                return Vec::new();
            }
        }
        Key::of(event).map_or_else(Vec::new, |key| self.press(key))
    }

    /// Does what `key` does on the screen, or in the overlay over it.
    fn press(&mut self, key: Key) -> Vec<Effect> {
        if let Overlay::Edit(_) = self.overlay {
            return self.edit_key(key);
        }
        // s also answers a one-server catalogue, which help leaves it out for.
        let action = keys::find(self.table(), key, |action| action == Action::Servers || self.offers(action));
        if matches!(self.overlay, Overlay::ConfirmStop) {
            return self.confirm(action);
        }
        let Some(action) = action else { return Vec::new() };
        match (action, &self.screen) {
            (Action::Quit, _) => return self.quit(),
            (Action::Abort, _) => return self.interrupt(),
            (Action::Help, _) => self.help = !self.help,
            (Action::Page, _) => self.page(key),
            (_, Screen::Setup) => return self.setup_key(action, key),
            (_, Screen::Chooser(_)) => self.chooser_key(action, key),
            (_, Screen::SignIn(_)) => return self.sign_in_key(action),
            (_, Screen::Run) => return self.run_key(action, key),
        }
        Vec::new()
    }

    /// Quits, a running test first stopping.
    fn quit(&mut self) -> Vec<Effect> {
        if !self.running() {
            return vec![Effect::Quit];
        }
        if std::mem::replace(&mut self.quitting, true) {
            return Vec::new();
        }
        self.overlay = Overlay::None;
        self.notice = "Stopping the test before quitting… ctrl+c quits at once.".into();
        Effect::command(Command::Stop)
    }

    /// Quits as a caught signal ending with `status` asks: like ctrl+c.
    pub fn signal(&mut self, status: u8) -> Vec<Effect> {
        self.caught = Some(status);
        self.interrupt()
    }

    /// Quits as ctrl+c asks: like q, and at once while quitting.
    fn interrupt(&mut self) -> Vec<Effect> {
        if !self.running() {
            return vec![Effect::Quit];
        }
        self.interrupted = true;
        if self.quitting { vec![Effect::Quit] } else { self.quit() }
    }

    /// Checks the paths once their settings have rested.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        self.now = now;
        match self.recheck {
            Some(due) if due <= now => {
                self.recheck = None;
                self.asked = Some(self.config.key());
                Effect::command(Command::Check(self.config.clone()))
            }
            _ => Vec::new(),
        }
    }

    /// Whether frames change without input: a spinner turns, a recheck waits or the checked paths turned stale.
    pub fn animating(&self) -> bool {
        let stale = self.stale.is_some_and(|at| at < self.now);
        stale || self.checking() || self.running() || matches!(self.screen, Screen::SignIn(_))
    }

    /// How the interface ends now: a signal's status after an interrupted run, or a caught one after a stopped run.
    pub fn exit(self) -> Exit {
        let outcome = self.view.run.as_ref().and_then(|run| run.outcome);
        let report = outcome.is_some() && matches!(self.screen, Screen::Run);
        let interrupted = self.interrupted || self.caught.is_some() && outcome == Some(Outcome::Stopped);
        Exit {
            view: self.view,
            palette: self.palette,
            signal: interrupted.then(|| self.caught.unwrap_or(INTERRUPTED)),
            report,
        }
    }

    /// Whether the wheel scrolls: not on the sign-in screen or in the editor and stop prompt.
    fn wheels(&self) -> bool {
        let overlay = matches!(self.overlay, Overlay::None | Overlay::Details);
        overlay && !matches!(self.screen, Screen::SignIn(_))
    }

    fn running(&self) -> bool {
        self.view.run.as_ref().is_some_and(|run| run.outcome.is_none())
    }

    /// The key table of the screen, or of the overlay over it.
    fn table(&self) -> &'static [keys::Binding] {
        match (&self.screen, &self.overlay) {
            (_, Overlay::Edit(_)) => keys::EDIT,
            (_, Overlay::Details) => keys::DETAILS,
            (_, Overlay::ConfirmStop) => keys::CONFIRM,
            (Screen::Setup, _) => keys::SETUP,
            (Screen::Chooser(_), _) => keys::CHOOSER,
            (Screen::SignIn(_), _) => keys::SIGN_IN,
            (Screen::Run, _) => keys::RUN,
        }
    }

    fn offers(&self, action: Action) -> bool {
        match action {
            Action::Available => self.can_use_available(),
            Action::Servers => self.view.catalogue.len() > 1,
            Action::Latency => self.view.servers.len() > 1,
            Action::Stop => self.running(),
            Action::Again | Action::Setup => !self.running(),
            _ => true,
        }
    }

    fn page(&mut self, key: Key) {
        use crossterm::event::KeyCode;
        self.follow = false;
        self.scroll = match key {
            Key::Code(KeyCode::PageUp) => self.scroll.saturating_sub(self.rows),
            Key::Code(KeyCode::PageDown) => self.scroll.saturating_add(self.rows),
            Key::Code(KeyCode::Home) => 0,
            _ => usize::MAX,
        };
    }

    /// Runs the settings when they are valid.
    fn start(&mut self) -> Vec<Effect> {
        if self.config.url.is_none() {
            self.ask_for_server();
            return Vec::new();
        }
        if let Err(error) = self.config.validate() {
            self.notice = format!("Test cannot start: {error}.");
            self.setup.row = self.first_stage();
            return Vec::new();
        }
        (self.recheck, self.asked) = (None, Some(self.config.key()));
        (self.screen, self.overlay, self.scroll) = (Screen::Run, Overlay::None, 0);
        self.view.run = Some(Run::default());
        self.notice = "Checking paths before the test. Press esc to stop.".into();
        Effect::command(Command::Run(self.config.clone()))
    }
}
