//! The terminal interface: the terminal's setup and restore, the event loop over any `Terminal` backend, and the
//! `App` whose screens it draws.
mod dialogs;
mod frame;
mod keys;
mod paths;
mod settings;
mod setup;
pub mod theme;

use crate::{
    config::{Config, PrepKey},
    controller::{Command, Controller},
    events::{Event, Events, Run, View},
    report,
    text::Profile,
};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as Input,
        EventStream, KeyEvent, KeyEventKind, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use dialogs::Chooser;
use futures_util::{Stream, StreamExt};
use graphite_meter_net::Pool;
use keys::{Action, Key};
use ratatui_core::{backend::Backend, terminal::Terminal};
use settings::Editor;
use setup::Setup;
use std::{
    io,
    pin::pin,
    sync::Arc,
    time::{Duration, Instant},
};
use theme::Palette;
use tokio::sync::mpsc::UnboundedReceiver;

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
    SignIn,
    Run,
}

enum Overlay {
    None,
    Edit(Editor),
}

/// What the interface asks of its loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Command(Box<Command>),
    Quit,
}

impl Effect {
    fn command(command: Command) -> Vec<Self> {
        vec![Self::Command(Box::new(command))]
    }
}

/// How the interface ended: what it showed, and the signal that stopped a run.
pub struct Exit {
    pub view: View,
    pub signal: Option<u8>,
}

/// The interface's state; time enters through each call.
pub struct App {
    config: Config,
    view: View,
    profile: Profile,
    palette: Palette,
    screen: Screen,
    overlay: Overlay,
    setup: Setup,
    help: bool,
    notice: String,
    /// When the paths are checked next.
    recheck: Option<Instant>,
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
            screen: Screen::Setup,
            overlay: Overlay::None,
            setup: Setup::default(),
            help: false,
            notice: String::new(),
            recheck: Some(now),
            asked: None,
            checked: None,
            scroll: 0,
            follow: false,
            rows: 0,
            since: now,
            now,
        }
    }

    pub fn event(&mut self, event: &Event, now: Instant) {
        self.now = now;
        self.view.apply(event);
        match event {
            Event::Prepared { .. } => {
                self.checked = self.asked.clone().map(|key| (key, now));
                if std::mem::take(&mut self.setup.chooser) {
                    self.open_chooser();
                }
            }
            Event::CheckFailed(_) => self.setup.chooser = false,
            Event::SignIn(_) => (self.screen, self.overlay) = (Screen::SignIn, Overlay::None),
            Event::SignInEnded(_) if matches!(self.screen, Screen::SignIn) => {
                self.screen = if self.running() { Screen::Run } else { Screen::Setup };
            }
            Event::RunFinished { error, .. } => {
                if let Some(reason) = report::unstarted(&self.view) {
                    (self.screen, self.notice) = (Screen::Setup, reason);
                    self.recheck = error.is_some().then(|| now + RECHECK);
                }
            }
            _ => {}
        }
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
            Input::Mouse(mouse) if matches!(self.overlay, Overlay::None) => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(3),
                _ => {}
            },
            _ => {}
        }
        Vec::new()
    }

    pub fn key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        self.now = now;
        let Some(key) = Key::of(key) else { return Vec::new() };
        if matches!(self.overlay, Overlay::Edit(_)) {
            return self.edit_key(key);
        }
        let table = match self.screen {
            Screen::Setup => keys::SETUP,
            Screen::Chooser(_) => keys::CHOOSER,
            Screen::SignIn => keys::SIGN_IN,
            Screen::Run => keys::RUN,
        };
        let Some(action) = keys::find(table, key, |action| self.offers(action)) else {
            return Vec::new();
        };
        match (action, &self.screen) {
            (Action::Quit | Action::Abort, _) => return vec![Effect::Quit],
            (Action::Help, _) => self.help = !self.help,
            (Action::Page, _) => self.page(key),
            (_, Screen::Setup) => return self.setup_key(action, key),
            (_, Screen::Chooser(_)) => self.chooser_key(action, key),
            (Action::Cancel, _) => {
                self.notice = "Sign-in canceled. Press v to request a new code.".into();
                return Effect::command(Command::Stop);
            }
            (Action::Stop, _) => {
                self.notice = "Stopping the test…".into();
                return Effect::command(Command::Stop);
            }
            (Action::Again, _) => return self.start(),
            (Action::Setup, _) => {
                (self.screen, self.notice, self.scroll) = (Screen::Setup, String::new(), 0);
                self.recheck_soon();
            }
            _ => {}
        }
        Vec::new()
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

    /// Whether frames change without input: a spinner turns or a recheck waits.
    pub fn animating(&self) -> bool {
        self.checking() || self.running() || matches!(self.screen, Screen::SignIn)
    }

    pub fn into_view(self) -> View {
        self.view
    }

    fn running(&self) -> bool {
        self.view.run.as_ref().is_some_and(|run| run.outcome.is_none())
    }

    fn offers(&self, action: Action) -> bool {
        match action {
            Action::Available => self.can_use_available(),
            Action::Servers => self.view.catalogue.len() != 1,
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

/// Runs the interface on this terminal until it quits.
pub async fn interactive(config: Config, runtimes: Arc<Pool>, stop: impl Future<Output = u8>) -> io::Result<Exit> {
    let profile = theme::profile(true, |name| std::env::var(name).ok());
    let _session = Session::enter()?;
    let mut terminal = Terminal::new(ratatui_crossterm::CrosstermBackend::new(io::stdout()))?;
    let (events, received) = Events::channel();
    let controller = Controller::new(true, runtimes, events);
    let input = EventStream::new().filter_map(|input| std::future::ready(input.ok()));
    let app = App::new(config, profile, Instant::now());
    run(&mut terminal, app, controller, received, input, stop).await
}

/// Draws `app` and feeds it events, input and ticks until it quits; `stop` quits as a signal, which stops a run.
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    mut controller: Controller,
    mut events: UnboundedReceiver<Event>,
    input: impl Stream<Item = Input>,
    stop: impl Future<Output = u8>,
) -> Result<Exit, B::Error> {
    let (mut input, mut stop, mut signal) = (pin!(input), pin!(stop), None);
    let mut frames = tokio::time::interval(FRAME);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut changed = true;
    loop {
        let effects = tokio::select! {
            Some(event) = events.recv() => {
                app.event(&event, Instant::now());
                changed = true;
                Vec::new()
            }
            input = input.next() => {
                changed = true;
                input.map_or_else(|| vec![Effect::Quit], |input| app.input(input, Instant::now()))
            }
            _ = frames.tick() => {
                let now = Instant::now();
                if std::mem::take(&mut changed) || app.animating() {
                    terminal.draw(|frame| app.draw(frame.buffer_mut(), now))?;
                }
                app.tick(now)
            }
            code = &mut stop, if signal.is_none() => {
                signal = Some(app.running().then_some(code));
                vec![Effect::Quit]
            }
        };
        for effect in effects {
            match effect {
                Effect::Command(command) => controller.command(*command),
                Effect::Quit => return Ok(Exit { view: app.into_view(), signal: signal.flatten() }),
            }
        }
    }
}

/// The terminal in raw mode on the alternate screen, reporting the mouse and pastes; dropping it restores the terminal.
struct Session;

impl Session {
    fn enter() -> io::Result<Self> {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            hook(info);
        }));
        enable_raw_mode()?;
        let session = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        let _ = execute!(io::stdout(), EnableBracketedPaste);
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
