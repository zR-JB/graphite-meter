//! The terminal interface: the console and what opens over it, the `App` the event loop in `terminal` draws.
mod chart;
pub mod chrome;
mod console;
mod dial;
mod dialogs;
mod frame;
mod keys;
mod paint;
mod paths;
mod run;
mod settings;
mod setup;
mod terminal;

pub use terminal::interactive;

use crate::{
    INTERRUPTED,
    config::{Config, PrepKey},
    controller::Command,
    events::{Check, Event, SignInEnd, View},
    format,
    model::{Outcome, Stage},
    net::approval,
    report,
    style::Palette,
    text::{self, Line, Profile, Style},
};
use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use dialogs::{Chooser, SignIn};
use graphite_meter_proto::origin::Origin;
use keys::{Action, Key};
use ratatui_core::layout::{Position, Rect};
use settings::Editor;
use setup::{Row, Setup};
use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

/// How long settings rest before their paths are checked again.
const RECHECK: Duration = Duration::from_millis(350);
/// Animation repaints at 120 fps, so the needle and readouts glide on fast displays; input paints at once.
const FRAME: Duration = Duration::from_micros(8_333);
/// How long the spinner shows each of its glyphs.
const SPIN: Duration = Duration::from_millis(80);
/// How long a notice stays.
const TOAST: Duration = Duration::from_millis(3200);
/// The smallest terminal the interface draws in, in columns and rows.
const SMALLEST: (u16, u16) = (60, 16);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// What opens over the console, with the state it keeps while open.
enum Overlay {
    None,
    /// The row being typed into, and whether a reset awaits its confirmation.
    Settings {
        editor: Option<Editor>,
        resetting: bool,
    },
    Details,
    Help,
    Chooser(Chooser),
    SignIn(SignIn),
    /// The server's address, asked for at a first start or after its check could not reach it.
    Connect(Editor),
}

impl Overlay {
    const SETTINGS: Self = Self::Settings { editor: None, resetting: false };
}

/// What a press on a drawn region does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The run key: start, or stop while a run goes.
    Key,
    /// A stage's chip: switches it on or off between runs.
    Chip(Stage),
    /// A key cap in a bar, which does what its key does.
    Cap(Action),
    /// The server in the top bar, which opens its settings.
    Server,
    /// A settings row, and a chooser entry.
    Row(usize),
    Entry(usize),
    /// The sign-in page's address.
    Link,
    /// The connect card's switch that remembers the server for later starts.
    Remember,
    /// A sheet's own ground, and the dimmed console around it, which closes it.
    Sheet,
    Backdrop,
}

/// What the interface asks of its loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Command(Box<Command>),
    /// Opens the sign-in page at this address in a browser.
    OpenBrowser(String),
    /// Keeps this server for the next start.
    Remember(Origin),
    /// Deletes the server kept for the next start.
    Forget,
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

/// A notice in the bottom bar until it lapses.
struct Toast {
    text: Line,
    until: Instant,
}

/// The interface's state; time enters through each call.
pub struct App {
    config: Config,
    view: View,
    profile: Profile,
    palette: Palette,
    overlay: Overlay,
    setup: Setup,
    live: run::Live,
    toast: Option<Toast>,
    /// Whether a stop was asked for and the run has not ended yet.
    stopping: bool,
    /// Whether the interface quits once the run stops.
    quitting: bool,
    /// Whether ctrl+c or a signal arrived during a run.
    interrupted: bool,
    /// The status of the latest caught signal.
    caught: Option<u8>,
    /// When the paths are checked next.
    recheck: Option<Instant>,
    /// What the latest operation prepared for.
    asked: Option<PrepKey>,
    /// Whether the server is kept for later starts: it was at start, or the user turned it on; and the one kept.
    remembering: bool,
    kept: Option<Origin>,
    /// A sheet's first shown line, and how many it shows.
    scroll: usize,
    rows: usize,
    /// The last frame's pressable regions, the topmost last, and the one the pointer rests on.
    hits: Vec<(Rect, Target)>,
    hover: Option<Target>,
    /// The last frame's size.
    area: Rect,
    /// The dial's ring for the area it was last drawn in.
    ring: RefCell<Option<dial::Ring>>,
    since: Instant,
    now: Instant,
}

impl App {
    /// The console for `config` in `profile` and `palette`, with the server `kept` for later starts if one is; its first
    /// tick checks the paths, or it asks for the server without one.
    pub fn new(config: Config, kept: Option<Origin>, (profile, palette): (Profile, Palette), now: Instant) -> Self {
        let mut app = Self {
            config,
            view: View::default(),
            profile,
            palette,
            overlay: Overlay::None,
            setup: Setup::default(),
            live: run::Live::default(),
            toast: None,
            stopping: false,
            quitting: false,
            interrupted: false,
            caught: None,
            recheck: None,
            asked: None,
            remembering: kept.is_some(),
            kept,
            scroll: 0,
            rows: 0,
            hits: Vec::new(),
            hover: None,
            area: Rect::default(),
            ring: RefCell::default(),
            since: now,
            now,
        };
        match app.config.url {
            Some(_) => app.recheck = Some(now),
            None => app.ask_for_server(),
        }
        app
    }

    /// Remembers the server for later starts, keeping a ready one at once, or forgets it, deleting the file.
    pub(super) fn remember(&mut self, on: bool) -> Vec<Effect> {
        if on == self.remembering {
            return Vec::new();
        }
        self.remembering = on;
        if !on {
            self.kept = None;
            return vec![Effect::Forget];
        }
        if self.view.check == Check::Ready && self.config.url.is_some() {
            self.kept.clone_from(&self.config.url);
            return self.kept.clone().map(Effect::Remember).into_iter().collect();
        }
        Vec::new()
    }

    /// Opens the server's address for typing, with the one set so far.
    fn ask_for_server(&mut self) {
        let address = self.config.url.as_ref().map(ToString::to_string).unwrap_or_default();
        self.overlay = Overlay::Connect(Editor::new(Row::Address, &address));
    }

    /// The address or setting being typed.
    fn editor(&self) -> Option<&Editor> {
        match &self.overlay {
            Overlay::Settings { editor: Some(editor), .. } | Overlay::Connect(editor) => Some(editor),
            _ => None,
        }
    }

    fn editor_mut(&mut self) -> Option<&mut Editor> {
        match &mut self.overlay {
            Overlay::Settings { editor: Some(editor), .. } | Overlay::Connect(editor) => Some(editor),
            _ => None,
        }
    }

    /// Shows `text` in the bottom bar for a while.
    fn notify(&mut self, text: Line) {
        self.toast = Some(Toast { text, until: self.now + TOAST });
    }

    /// Takes `event`; a run ending while the interface quits ends it.
    pub fn event(&mut self, event: &Event, now: Instant) -> Vec<Effect> {
        self.now = now;
        let shown = self.latency_server().cloned();
        self.view.apply(event);
        // A run refused at its start leaves the shown run, and its needle, as they were.
        if !matches!(event, Event::RunFinished { .. }) || self.view.refused.is_none() {
            self.live.event(event, self.view.run.as_ref(), shown.as_ref(), now);
        }
        match event {
            Event::Prepared { servers, .. } => {
                if std::mem::take(&mut self.setup.chooser) {
                    self.open_chooser();
                }
                // Only the address this check was for, which a later edit may already have replaced.
                let current = self.asked == Some(self.config.key());
                let prepared = current && !servers.is_empty() && servers.iter().all(|server| server.path.is_ok());
                if current
                    && matches!(self.overlay, Overlay::Connect(_))
                    && servers.iter().any(|server| server.path.is_ok())
                {
                    self.overlay = Overlay::None;
                    let ready = self.labels().join(", ");
                    self.notify(Line::styled("Ready  ", self.palette.ok).and(ready, self.palette.text));
                }
                if current && !self.running() && servers.iter().all(|server| server.path.is_err()) {
                    self.unreachable();
                }
                if prepared && self.remembering && self.config.url != self.kept {
                    self.kept.clone_from(&self.config.url);
                    return self.kept.clone().map(Effect::Remember).into_iter().collect();
                }
            }
            Event::CheckFailed(_) => {
                self.setup.chooser = false;
                // A sign-in that lapsed leaves the address as it is; a server that could not be reached opens it.
                if !self.running() && matches!(self.view.check, Check::Failed(_)) {
                    self.unreachable();
                }
            }
            Event::SignIn(_) => {
                self.overlay = Overlay::SignIn(SignIn { since: now, opened: false });
            }
            Event::SignInEnded(end) => {
                if matches!(self.overlay, Overlay::SignIn(_)) {
                    self.overlay = Overlay::None;
                }
                match end {
                    SignInEnd::Approved => self.notify(Line::styled("Signed in", self.palette.ok)),
                    SignInEnd::Expired => self.notify(Line::styled(approval::EXPIRED, self.palette.warn)),
                    SignInEnd::Failed | SignInEnd::Canceled => {}
                }
            }
            Event::RunStarted { .. } => self.stopping = false,
            Event::ServerFailed(failure) => {
                let name = self.view.servers.iter().find(|path| path.id == failure.server);
                let name = name.map_or(failure.server.as_str(), |path| path.name.as_str());
                let text = format!("{name}: {}", failure.failure.reason.label());
                self.notify(Line::styled(text, self.palette.warn));
            }
            Event::RunFinished { error, .. } => {
                self.stopping = false;
                if let Some(reason) = report::unreported(&self.view) {
                    self.notify(Line::styled(reason, self.palette.warn));
                    self.recheck = error.is_some().then(|| now + RECHECK);
                }
                if self.quitting {
                    return vec![Effect::Quit];
                }
            }
            Event::Checking(_)
            | Event::StageStarted(_)
            | Event::Measuring(_)
            | Event::Sample { .. }
            | Event::Probes { .. }
            | Event::StageFinished(_) => {}
        }
        Vec::new()
    }

    /// Opens the address again after its check found no server to run with, unless something else is open.
    fn unreachable(&mut self) {
        if matches!(self.overlay, Overlay::None) && self.config.url.is_some() {
            self.ask_for_server();
        }
    }

    /// Reacts to terminal input: keys, pasted text, and the pointer's moves, presses and wheel.
    pub fn input(&mut self, input: Input, now: Instant) -> Vec<Effect> {
        self.now = now;
        match input {
            Input::Key(key) if key.kind != KeyEventKind::Release => return self.key(key, now),
            Input::Paste(text) => {
                // An address pasted over the console is the server to test.
                if matches!(self.overlay, Overlay::None) && !self.running() {
                    self.ask_for_server();
                    self.editor_mut().map(Editor::clear);
                }
                if let Some(editor) = self.editor_mut() {
                    editor.insert(text.trim());
                }
            }
            Input::Mouse(mouse) => return self.mouse(mouse),
            _ => {}
        }
        Vec::new()
    }

    /// The pointer: a move lights what it rests on, a press presses it, the wheel scrolls a sheet or moves its focus.
    fn mouse(&mut self, mouse: MouseEvent) -> Vec<Effect> {
        let target = self.target(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => self.hover = target,
            MouseEventKind::Down(MouseButton::Left) => {
                return target.map_or_else(Vec::new, |target| self.click(target));
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let step = if mouse.kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                match self.overlay {
                    Overlay::Settings { .. } => self.move_focus(step),
                    Overlay::Details | Overlay::Help => self.scroll = self.scroll.saturating_add_signed(3 * step),
                    Overlay::Chooser(_) => {
                        let key = if step < 0 { KeyCode::Up } else { KeyCode::Down };
                        self.chooser_key(Action::Move, Key::Code(key));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// The topmost pressable region at `column` and `row` in the last frame.
    fn target(&self, column: u16, row: u16) -> Option<Target> {
        let at = Position::new(column, row);
        self.hits
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, target)| *target)
    }

    /// Whether the pointer rests on `target`.
    pub fn hovered(&self) -> Option<Target> {
        self.hover
    }

    fn click(&mut self, target: Target) -> Vec<Effect> {
        match target {
            Target::Key if self.running() => return self.stop(),
            Target::Key => return self.start(),
            Target::Chip(stage) => self.switch(stage),
            // A cap pressed while typing does what its key does in the editor, and any other cap leaves the editor.
            Target::Cap(Action::Apply) if self.editor().is_some() => return self.edit_key(Key::Code(KeyCode::Enter)),
            Target::Cap(Action::Cancel) if self.editor().is_some() => return self.edit_key(Key::Code(KeyCode::Esc)),
            Target::Cap(action) => {
                if let Overlay::Settings { editor, .. } = &mut self.overlay {
                    *editor = None;
                }
                return self.act(action, Key::Code(KeyCode::Null));
            }
            Target::Server => self.open_settings(Some(Row::Address)),
            Target::Row(index) => return self.click_row(index),
            Target::Entry(index) => self.click_entry(index),
            Target::Link => {
                if let Some(prompt) = &self.view.sign_in {
                    if let Overlay::SignIn(sign_in) = &mut self.overlay {
                        sign_in.opened = true;
                    }
                    return vec![Effect::OpenBrowser(prompt.url.clone())];
                }
            }
            Target::Remember => return self.remember(!self.remembering),
            // The sign-in card stays while its sign-in waits.
            Target::Backdrop if matches!(self.overlay, Overlay::SignIn(_)) => {}
            Target::Backdrop => self.overlay = Overlay::None,
            Target::Sheet => {}
        }
        Vec::new()
    }

    pub fn key(&mut self, event: KeyEvent, now: Instant) -> Vec<Effect> {
        self.now = now;
        Key::of(event).map_or_else(Vec::new, |key| self.press(key))
    }

    /// Does what `key` does in the context on top.
    fn press(&mut self, key: Key) -> Vec<Effect> {
        if self.editor().is_some() {
            return self.edit_key(key);
        }
        let action = keys::find(self.table(), key, |action| self.offers(action));
        action.map_or_else(Vec::new, |action| self.act(action, key))
    }

    fn act(&mut self, action: Action, key: Key) -> Vec<Effect> {
        match action {
            Action::Quit => return self.quit(),
            Action::Abort => return self.interrupt(),
            Action::Help if matches!(self.overlay, Overlay::Help) => self.overlay = Overlay::None,
            Action::Help => (self.overlay, self.scroll) = (Overlay::Help, 0),
            Action::Settings => self.open_settings(None),
            Action::Details => (self.overlay, self.scroll) = (Overlay::Details, 0),
            Action::Close if matches!(self.overlay, Overlay::Settings { .. } | Overlay::Details | Overlay::Help) => {
                self.overlay = Overlay::None;
            }
            // The chooser closes back to the settings, as esc does.
            Action::Close if matches!(self.overlay, Overlay::Chooser(_)) => self.overlay = Overlay::SETTINGS,
            Action::Start => return self.start(),
            Action::Stop => return self.stop(),
            Action::Stage => {
                let index = match key {
                    Key::Code(KeyCode::Char(digit @ '1'..='9')) => digit as usize - '1' as usize,
                    _ => return Vec::new(),
                };
                if let Some(&stage) = Stage::ALL.get(index) {
                    self.switch(stage);
                }
            }
            Action::Latency => self.next_latency_server(),
            Action::Recheck => {
                self.recheck_soon();
                self.notify(Line::styled("Checking the server again…", self.palette.muted));
            }
            Action::Scroll | Action::Page => self.page(key),
            _ => match self.overlay {
                Overlay::Settings { .. } => return self.settings_key(action, key),
                Overlay::Chooser(_) => self.chooser_key(action, key),
                Overlay::SignIn(_) => return self.sign_in_key(action),
                _ => {}
            },
        }
        Vec::new()
    }

    /// Switches `stage` on or off for the next run.
    fn switch(&mut self, stage: Stage) {
        if self.running() {
            return;
        }
        let on = !self.config.stages.contains(&stage);
        self.set_flag(Row::Stage(stage), on);
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
        self.notify(Line::styled(
            "Stopping the test before quitting… Ctrl+C quits at once.",
            self.palette.muted,
        ));
        self.stop()
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

    /// Stops the run at once.
    fn stop(&mut self) -> Vec<Effect> {
        if !self.running() || std::mem::replace(&mut self.stopping, true) {
            return Vec::new();
        }
        Effect::command(Command::Stop)
    }

    /// Checks the paths once their settings have rested.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        self.now = now;
        self.toast = self.toast.take().filter(|toast| toast.until > now);
        // A check waits for the run to end: the newest operation would replace the run.
        match self.recheck {
            Some(due) if due <= now && !self.running() => {
                self.recheck = None;
                self.asked = Some(self.config.key());
                Effect::command(Command::Check(self.config.clone()))
            }
            _ => Vec::new(),
        }
    }

    /// Whether a frame tick has work: something animates, or a check is due.
    pub fn waiting(&self) -> bool {
        self.animating() || self.recheck.is_some()
    }

    /// Whether frames change without input: a spinner turns, the needle moves, or a notice waits to lapse.
    pub fn animating(&self) -> bool {
        let busy = self.checking() || self.running() || matches!(self.overlay, Overlay::SignIn(_));
        busy || self.toast.is_some() || self.live.moving(self.now)
    }

    /// How the interface ends now: a signal's status after an interrupted run, or a caught one after a stopped run.
    pub fn exit(self) -> Exit {
        let outcome = self.view.latest().and_then(|run| run.outcome);
        let report = outcome.is_some() && self.view.run.as_ref().is_some_and(|run| run.at.is_some());
        let interrupted = self.interrupted || self.caught.is_some() && outcome == Some(Outcome::Stopped);
        Exit {
            view: self.view,
            palette: self.palette,
            signal: interrupted.then(|| self.caught.unwrap_or(INTERRUPTED)),
            report,
        }
    }

    /// The key table of what is on top.
    fn table(&self) -> &'static [keys::Binding] {
        match &self.overlay {
            Overlay::None | Overlay::Connect(_) => keys::CONSOLE,
            Overlay::Settings { .. } => keys::SETTINGS_SHEET,
            Overlay::Details => keys::DETAILS_SHEET,
            Overlay::Help => keys::HELP_SHEET,
            Overlay::Chooser(_) => keys::CHOOSER,
            Overlay::SignIn(_) => keys::SIGN_IN,
        }
    }

    fn offers(&self, action: Action) -> bool {
        match action {
            Action::Stop => self.running(),
            Action::Start | Action::Stage => !self.running(),
            Action::Latency => self.participants().len() > 1,
            Action::Recheck => self.config.url.is_some() && !self.running(),
            _ => true,
        }
    }

    fn page(&mut self, key: Key) {
        let rows = self.rows.max(1);
        self.scroll = match key {
            Key::Code(KeyCode::PageUp) => self.scroll.saturating_sub(rows),
            Key::Code(KeyCode::PageDown) => self.scroll.saturating_add(rows),
            Key::Code(KeyCode::Home) => 0,
            Key::Code(KeyCode::End) => usize::MAX,
            key => self.scroll.saturating_add_signed(key.step()),
        };
    }

    /// Runs the settings when they are valid; without a server it asks for one.
    fn start(&mut self) -> Vec<Effect> {
        if self.config.url.is_none() {
            self.ask_for_server();
            return Vec::new();
        }
        if let Err(invalid) = self.config.validate() {
            let text = format!("{}.", text::capitalized(&invalid.to_string()));
            self.notify(Line::styled(text, self.palette.warn));
            return Vec::new();
        }
        (self.recheck, self.asked) = (None, Some(self.config.key()));
        (self.overlay, self.stopping) = (Overlay::None, false);
        self.view.starting = true;
        Effect::command(Command::Run(self.config.clone()))
    }

    /// The spinner's glyph now.
    fn spinner(&self) -> &'static str {
        let frames = self.now.saturating_duration_since(self.since).as_millis() / SPIN.as_millis();
        SPINNER[(frames % SPINNER.len() as u128) as usize]
    }

    pub(super) fn checkbox(&self, on: bool) -> Line {
        match on {
            true => Line::styled("●", self.palette.accent),
            false => Line::styled("○", self.palette.muted),
        }
    }

    /// The selected servers' names with their locations.
    pub(super) fn labels(&self) -> Vec<String> {
        let label = |server: &crate::run::prepare::ServerPath| format::server(&server.name, &server.location);
        self.view.servers.iter().map(label).collect()
    }

    /// A key and what it does as a bar's cap: the key on a raised plate, its deed beside it.
    fn cap(&self, key: &str, does: &str, hovered: bool) -> Line {
        let plate = Style {
            bg: Some(if hovered { self.palette.selected } else { self.palette.raised }),
            ..self.palette.text
        };
        Line::styled(format!(" {key} "), plate).and(format!(" {does}"), self.palette.muted)
    }
}
