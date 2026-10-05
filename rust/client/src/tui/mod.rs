//! The terminal interface: the `App` whose screens the event loop in `terminal` draws.
mod chart;
pub mod chrome;
mod dialogs;
mod frame;
mod keys;
mod paths;
mod run;
mod settings;
mod setup;
mod terminal;
pub mod theme;
mod track;

pub use terminal::{interactive, run};

use crate::{
    INTERRUPTED,
    config::{Config, PrepKey},
    controller::Command,
    events::{Event, Run, SignInEnd, View},
    net::approval,
    report,
    text::Profile,
};
use crossterm::event::{Event as Input, KeyEvent, KeyEventKind, MouseEventKind};
use dialogs::{Chooser, SignIn};
use keys::{Action, Key};
use settings::Editor;
use setup::Setup;
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
    Quit,
}

impl Effect {
    fn command(command: Command) -> Vec<Self> {
        vec![Self::Command(Box::new(command))]
    }
}

/// How the interface ended: what it showed and in which palette, the signal that stopped a run, and whether it
/// showed a finished run, whose report follows.
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
    /// The signal that stopped the run.
    signal: Option<u8>,
    /// When the paths are checked next.
    recheck: Option<Instant>,
    /// When the checked paths turn stale, until a frame shows it.
    stale: Option<Instant>,
    /// What the latest operation prepared for, and when its paths arrived.
    asked: Option<PrepKey>,
    checked: Option<(PrepKey, Instant)>,
    /// The body's first shown line, whether it follows the focused row, and the lines it shows.
    scroll: usize,
    follow: bool,
    rows: usize,
    since: Instant,
    now: Instant,
}

impl App {
    /// Setup for `config` in `profile`; its first tick checks the paths.
    pub fn new(config: Config, profile: Profile, now: Instant) -> Self {
        Self {
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
            signal: None,
            recheck: Some(now),
            stale: None,
            asked: None,
            checked: None,
            scroll: 0,
            follow: false,
            rows: 0,
            since: now,
            now,
        }
    }

    /// Takes `event`; a run ending while the interface quits ends it.
    pub fn event(&mut self, event: &Event, now: Instant) -> Vec<Effect> {
        self.now = now;
        self.view.apply(event);
        self.live.event(event, now);
        match event {
            Event::Prepared { .. } => {
                (self.checked, self.stale) = (self.asked.clone().map(|key| (key, now)), Some(now + FRESH));
                if std::mem::take(&mut self.setup.chooser) {
                    self.open_chooser();
                }
            }
            Event::CheckFailed(_) => self.setup.chooser = false,
            Event::SignIn(_) => {
                (self.screen, self.overlay) = (Screen::SignIn(SignIn::new(now)), Overlay::None);
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
            Event::ServerFailed { server, failure, .. } => {
                let name = self.view.servers.iter().find(|path| path.id == *server);
                let name = name.map_or(server.as_str(), |path| path.name.as_str());
                self.notice = format!("{name}: {}", failure.reason.label());
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

    /// Reacts to terminal input: keys, pasted text and the mouse wheel.
    pub fn input(&mut self, input: Input, now: Instant) -> Vec<Effect> {
        match input {
            Input::Key(key) if key.kind != KeyEventKind::Release => return self.key(key, now),
            Input::Paste(text) => {
                if let Overlay::Edit(editor) = &mut self.overlay {
                    editor.insert(&text);
                }
            }
            Input::Mouse(mouse) if self.wheels() => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(3),
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
        let Some(key) = Key::of(event) else { return Vec::new() };
        let table = match (&self.screen, &self.overlay) {
            (_, Overlay::Edit(_)) => return self.edit_key(key),
            (_, Overlay::Details) => keys::DETAILS,
            (_, Overlay::ConfirmStop) => keys::CONFIRM,
            (Screen::Setup, _) => keys::SETUP,
            (Screen::Chooser(_), _) => keys::CHOOSER,
            (Screen::SignIn(_), _) => keys::SIGN_IN,
            (Screen::Run, _) => keys::RUN,
        };
        // s also answers a one-server catalogue, which help leaves it out for.
        let action = keys::find(table, key, |action| action == Action::Servers || self.offers(action));
        if matches!(self.overlay, Overlay::ConfirmStop) {
            return self.confirm(action);
        }
        let Some(action) = action else { return Vec::new() };
        match (action, &self.screen) {
            (Action::Quit, _) => return self.quit(),
            (Action::Abort, _) => return self.interrupt(INTERRUPTED),
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

    /// Quits as ctrl+c or a signal ending with `status` asks: like q, and at once while quitting.
    pub fn interrupt(&mut self, status: u8) -> Vec<Effect> {
        if !self.running() {
            return vec![Effect::Quit];
        }
        self.signal.get_or_insert(status);
        match self.quitting {
            true => vec![Effect::Quit],
            false => self.quit(),
        }
    }

    /// Eases the shown rates, and checks the paths once their settings have rested.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        self.now = now;
        if let Some(run) = &self.view.run {
            self.live.ease(run.rates, now);
        }
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

    /// How the interface ends now.
    pub fn exit(self) -> Exit {
        let finished = self.view.run.as_ref().is_some_and(|run| run.outcome.is_some());
        let report = finished && matches!(self.screen, Screen::Run);
        Exit {
            view: self.view,
            palette: self.palette,
            signal: self.signal,
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
