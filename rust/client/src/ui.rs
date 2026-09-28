//! Terminal input owns no measurement IO and never blocks its producer.
mod keys;
mod render;
mod setup;
use crate::{
    Error,
    config::Config,
    model::{Phase, Snapshot},
    report::{run_servers, server_name, terminal_char},
    theme::Theme,
    vocabulary::MISSING,
};
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute,
};
use futures_util::StreamExt;
use graphite_meter_core::{catalog::MAX_SELECTED_SERVERS, text::terminal_character as safe_character};
use keys::*;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap,
    },
};
use setup::{Edit, Kind};
use std::{
    io::{self, IsTerminal},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Exit {
    pub interrupted: bool,
    pub running: bool,
}

#[derive(Clone, Debug)]
pub enum Command {
    Run(Config),
    Verify(Config),
    Cancel,
    OpenBrowser,
}

const MAX_TEXT: usize = 4096;
const MAX_SERVERS: usize = 128;
/// As in the Go client, paths are checked again once path settings stop changing.
const RECHECK_DELAY: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CancelState {
    #[default]
    Idle,
    Confirming,
    Requested,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Popup {
    #[default]
    None,
    Servers,
    Details,
}

/// Which keys apply, as Go's handleKey picks a handler: a sign-in, the editor, a stop to
/// confirm, a popup, the reset prompt, or the run and setup views.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Auth,
    Edit,
    Confirm,
    Servers,
    Details,
    Reset,
    Main,
}

/// A panel's scroll position, clamped at each draw to the lines the panel hides.
#[derive(Clone, Copy, Debug, Default)]
struct Scroll {
    offset: u16,
    /// What the last draw left out, which the footer offers with PgDn.
    hidden: u16,
}

impl Scroll {
    /// Go's line and page keys; other keys leave the position.
    fn key(&mut self, code: KeyCode, page: u16) {
        self.offset = match code {
            KeyCode::Up | KeyCode::Char('k') => self.offset.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.offset.saturating_add(1),
            KeyCode::PageUp => self.offset.saturating_sub(page),
            KeyCode::PageDown => self.offset.saturating_add(page),
            _ => self.offset,
        };
    }

    /// Clamps the position to what `lines` hide in `visible` rows, as a paragraph scrolls.
    fn clamp(&mut self, lines: usize, visible: usize) -> (u16, u16) {
        self.hidden = u16::try_from(lines.saturating_sub(visible)).unwrap_or(u16::MAX);
        self.offset = self.offset.min(self.hidden);
        (self.offset, 0)
    }
}

/// Return paths, cancellation and partial initialization all restore the terminal.
/// Ratatui additionally installs its panic hook before entering raw mode.
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

struct TerminalSession {
    terminal: DefaultTerminal,
    theme: Theme,
    _restore: Restore,
}
impl TerminalSession {
    async fn enter() -> Result<Self, Error> {
        // Like Bubble Tea, crossterm reads keys from the terminal when stdin is redirected.
        if !io::stdout().is_terminal() {
            return Err("interactive mode requires a terminal on stdout".into());
        }
        let restore = Restore;
        let terminal = ratatui::try_init()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        // Raw mode keeps the answer off the screen, and it is read before crossterm reads keys;
        // the clear wipes whatever a terminal that ignores the query printed.
        let theme = Theme::ask().await;
        execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;
        Ok(Self {
            terminal,
            theme,
            _restore: restore,
        })
    }
}

pub async fn run(
    config: Config,
    mut snapshots: watch::Receiver<Snapshot>,
    commands: mpsc::Sender<Command>,
    mut interrupts: mpsc::Receiver<()>,
) -> Result<Exit, Error> {
    let mut session = TerminalSession::enter().await?;
    let mut chrome = render::Chrome::default();
    let mut ui = Ui::new(config, snapshots.borrow_and_update().clone());
    ui.theme = session.theme;
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_millis(33));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut snapshot_changed = false;
    loop {
        tokio::select! {
            changed = snapshots.changed() => {
                changed.map_err(|_| "measurement controller stopped")?;
                snapshot_changed = true;
            }
            Some(()) = interrupts.recv() => {
                if ui.interrupt(&commands) {
                    return Ok(ui.exit());
                }
                dirty = true;
            }
            _ = refresh.tick() => {
                if snapshot_changed {
                    ui.update(snapshots.borrow_and_update().clone());
                    snapshot_changed = false;
                    dirty = true;
                    if ui.quitting && !ui.running() {
                        return Ok(ui.exit());
                    }
                }
                dirty |= ui.recheck(&commands);
                if ui.active() {
                    ui.frame();
                    dirty = true;
                }
                if dirty {
                    session.terminal.draw(|frame| ui.draw(frame))?;
                    chrome.show(ui.title(), ui.progress())?;
                    dirty = false;
                }
            }
            event = events.next() => {
                let Some(event) = event else { return Err("terminal input closed".into()); };
                match event? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if ui.key(key, &commands) {
                            return Ok(ui.exit());
                        }
                        dirty = true;
                    }
                    Event::Paste(text) => {
                        ui.paste(&text);
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
        }
    }
}

#[derive(Default)]
struct Ui {
    config: Config,
    requested: Config,
    snapshot: Snapshot,
    /// The results a run again replaces, shown again if it never starts.
    previous: Option<Snapshot>,
    theme: Theme,
    advanced: bool,
    rows: ListState,
    servers: ListState,
    live: bool,
    /// A requested run that has not started: the check it replaces is not a return to setup.
    starting: bool,
    open_chooser: bool,
    servers_before: Vec<String>,
    popup: Popup,
    details_scroll: Scroll,
    auth_scroll: Scroll,
    /// Go scrolls the body; here the panel whose lines can overflow scrolls.
    body_scroll: Scroll,
    help: bool,
    edit: Option<Edit>,
    reset_prompt: bool,
    notice: String,
    recheck: Option<tokio::time::Instant>,
    awaiting: bool,
    cancel: CancelState,
    quitting: bool,
    interrupted: bool,
    latency_pick: Option<String>,
    received_at: Option<tokio::time::Instant>,
    shown_down: Option<f64>,
    shown_up: Option<f64>,
}
impl Ui {
    fn new(config: Config, snapshot: Snapshot) -> Self {
        Self {
            requested: config.clone(),
            config,
            rows: ListState::default().with_selected(Some(0)),
            servers: ListState::default().with_selected(Some(0)),
            received_at: Some(tokio::time::Instant::now()),
            shown_down: snapshot.latest.down_bps,
            shown_up: snapshot.latest.up_bps,
            snapshot,
            ..Self::default()
        }
    }
    fn update(&mut self, mut snapshot: Snapshot) {
        let was_live = self.live;
        if snapshot.auth != self.snapshot.auth {
            self.auth_scroll.offset = 0;
        }
        if snapshot.error != self.snapshot.error {
            self.notice.clear();
        }
        snapshot.servers.truncate(MAX_SERVERS);
        snapshot.server_latencies.truncate(MAX_SELECTED_SERVERS);
        snapshot.results.truncate(16);
        if self.live && self.snapshot.phase.live() && !snapshot.phase.live() && !self.quitting {
            self.notice.clear();
        }
        if self.live && !self.quitting && self.snapshot.participants.is_empty() && !snapshot.participants.is_empty() {
            self.notice = "Test started. Press esc to stop.".into();
        }
        let failed = snapshot
            .failures
            .get(self.snapshot.failures.len()..)
            .and_then(<[_]>::last);
        if let Some(failure) = failed.filter(|_| !self.quitting) {
            let name = server_name(&snapshot, &failure.server_id);
            self.notice = format!("{name}: {}", failure.reason.label());
        }
        self.awaiting = false;
        if snapshot.auth.is_some() || !snapshot.phase.live() {
            if self.cancel != CancelState::Idle {
                self.notice.clear();
            }
            self.cancel = CancelState::Idle;
        }
        if snapshot.stage != self.snapshot.stage {
            (self.shown_down, self.shown_up) = (None, None);
        }
        if snapshot.phase == Phase::Checking && !self.starting {
            self.live = false;
        }
        self.starting &= snapshot.phase == Phase::Checking;
        self.latency_pick = self
            .latency_pick
            .take()
            .filter(|pick| snapshot.participants.contains(pick));
        self.received_at = Some(tokio::time::Instant::now());
        // As in Go, a run that never starts leaves the last results, or setup, in place.
        if self.live
            && !self.quitting
            && matches!(snapshot.phase, Phase::Failed | Phase::Cancelled)
            && !snapshot.started()
        {
            if snapshot.phase == Phase::Cancelled {
                self.notice = "Test stopped before it started.".into();
            }
            match self.previous.take() {
                Some(previous) => {
                    snapshot = Snapshot {
                        error: snapshot.error,
                        ..previous
                    }
                }
                None => self.live = false,
            }
        }
        if self.live != was_live {
            self.body_scroll.offset = 0;
        }
        if snapshot.auth.is_some() || self.popup == Popup::Details && !self.live {
            if self.popup == Popup::Servers {
                self.config.servers = std::mem::take(&mut self.servers_before);
            }
            self.popup = Popup::None;
            self.details_scroll.offset = 0;
        }
        self.snapshot = snapshot;
        if self.open_chooser && !self.live && !self.checking() && !self.snapshot.servers.is_empty() {
            self.open_chooser = false;
            self.open_servers();
        }
    }
    fn frame(&mut self) {
        for (shown, target) in [
            (&mut self.shown_down, self.snapshot.latest.down_bps),
            (&mut self.shown_up, self.snapshot.latest.up_bps),
        ] {
            *shown = target.map(|target| shown.map_or(target, |value| value + 0.35 * (target - value)));
        }
    }
    fn elapsed(&self) -> Duration {
        let since = self.received_at.filter(|_| self.active()).map(|at| at.elapsed());
        self.snapshot.latest.elapsed + since.unwrap_or_default()
    }
    fn notice(&self) -> (&str, bool) {
        if self.cancel == CancelState::Confirming {
            ("Stop the test? esc confirms, any other key continues.", false)
        } else if self.cancel == CancelState::Requested {
            ("Stopping the test…", false)
        } else if !self.notice.is_empty() {
            (&self.notice, false)
        } else if let Some(error) = self.snapshot.error.as_deref() {
            (error, true)
        } else {
            ("", false)
        }
    }
    fn latency_server(&self) -> Option<&str> {
        self.latency_pick.as_deref().or(self.snapshot.latency_focus.as_deref())
    }
    fn active(&self) -> bool {
        self.awaiting || self.snapshot.phase.busy()
    }
    /// Setup paths are being checked, or will be once edits settle.
    fn checking(&self) -> bool {
        self.awaiting || self.recheck.is_some() || self.snapshot.phase == Phase::Checking
    }
    fn running(&self) -> bool {
        self.snapshot.auth.is_none() && (self.awaiting && self.live || self.starting || self.snapshot.phase.live())
    }
    fn exit(&self) -> Exit {
        Exit {
            interrupted: self.interrupted,
            running: self.running(),
        }
    }
    fn quit(&mut self, commands: &mpsc::Sender<Command>) -> bool {
        if !self.running() {
            return true;
        }
        self.send(Command::Cancel, commands);
        (self.quitting, self.cancel) = (true, CancelState::Idle);
        self.notice = "Stopping the test before quitting… ctrl+c quits at once.".into();
        false
    }
    fn interrupt(&mut self, commands: &mpsc::Sender<Command>) -> bool {
        self.interrupted |= self.running();
        self.quitting || self.quit(commands)
    }
    fn send(&mut self, command: Command, commands: &mpsc::Sender<Command>) -> bool {
        let requested = match &command {
            Command::Run(config) | Command::Verify(config) => Some(config.clone()),
            _ => None,
        };
        match commands.try_send(command) {
            Ok(()) => {
                if let Some(requested) = requested {
                    self.requested = requested;
                    self.cancel = CancelState::Idle;
                    self.recheck = None;
                }
                self.notice.clear();
                self.awaiting = true;
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.notice = "Controller is busy; try again.".into();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.notice = "Controller is unavailable.".into();
                false
            }
        }
    }
    /// Paths depend on these settings, so changing one checks them again.
    fn recheck_if_changed(&mut self, before: &Config) {
        if before.preparation_key() != self.config.preparation_key() {
            self.recheck_soon();
        }
    }
    fn recheck_soon(&mut self) {
        self.recheck = Some(tokio::time::Instant::now() + RECHECK_DELAY);
    }
    /// Sends a settled re-check, keeping the notice of the change behind it.
    fn recheck(&mut self, commands: &mpsc::Sender<Command>) -> bool {
        if self.recheck.is_none_or(|at| at > tokio::time::Instant::now()) {
            return false;
        }
        self.recheck = None;
        let notice = std::mem::take(&mut self.notice);
        if self.send(Command::Verify(self.config.clone()), commands) {
            self.notice = notice;
        }
        true
    }
    fn paste(&mut self, text: &str) {
        if self.snapshot.auth.is_none()
            && let Some(edit) = &mut self.edit
        {
            edit.insert(text);
        }
    }
    /// Which keys apply now.
    fn mode(&self) -> InputMode {
        match self.popup {
            _ if self.snapshot.auth.is_some() => InputMode::Auth,
            _ if self.edit.is_some() => InputMode::Edit,
            _ if self.cancel == CancelState::Confirming => InputMode::Confirm,
            Popup::Servers => InputMode::Servers,
            Popup::Details => InputMode::Details,
            Popup::None if self.reset_prompt => InputMode::Reset,
            Popup::None => InputMode::Main,
        }
    }
    /// Go's handleKey; true quits.
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        let code = key.code;
        if code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.interrupt(commands);
        }
        match self.mode() {
            InputMode::Auth => return self.auth_key(code, commands),
            InputMode::Edit => self.edit_key(key),
            _ if QUIT.matches(code) => return self.quit(commands),
            InputMode::Confirm => {
                self.cancel = CancelState::Idle;
                if !CONFIRM_STOP.matches(code) {
                    self.notice = "Test continues.".into();
                } else if self.send(Command::Cancel, commands) {
                    self.cancel = CancelState::Requested;
                }
            }
            _ if HELP.matches(code) => self.help = !self.help,
            InputMode::Servers => self.servers_key(code),
            InputMode::Details if CLOSE.matches(code) => self.popup = Popup::None,
            InputMode::Details => self.details_scroll.key(code, 10),
            // Enter or Space on the row confirms the reset; any other key keeps the settings.
            InputMode::Reset
                if pressed(code, &[ACTIVATE, TOGGLE]).is_none() || !matches!(self.field().kind, Kind::Reset) =>
            {
                self.reset_prompt = false;
                self.notice = "Settings kept.".into();
            }
            InputMode::Reset | InputMode::Main => self.main_key(code, commands),
        }
        false
    }
    fn auth_key(&mut self, code: KeyCode, commands: &mpsc::Sender<Command>) -> bool {
        match pressed(code, &SIGN_IN) {
            Some(QUIT) => return true,
            Some(OPEN) => {
                self.send(Command::OpenBrowser, commands);
            }
            Some(CANCEL) if self.send(Command::Cancel, commands) => {
                self.live = false;
                self.notice = "Sign-in canceled. Press v to request a new code.".into();
            }
            _ => self.auth_scroll.key(code, 4),
        }
        false
    }
    fn edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = &mut self.edit else { return };
        match pressed(key.code, &EDIT) {
            Some(DISCARD) => self.edit = None,
            Some(APPLY) => {
                let (field, value, before) = (edit.field, edit.text(), self.config.clone());
                match self.apply(field, value) {
                    Ok(()) => {
                        self.edit = None;
                        self.notice.clear();
                        self.recheck_if_changed(&before);
                    }
                    Err(error) => self.notice = error.to_string(),
                }
            }
            _ if !key.modifiers.contains(KeyModifiers::CONTROL) => edit.key(key.code),
            _ => {}
        }
    }
    /// The chooser edits the selection in place until Enter applies or Esc restores it.
    fn servers_key(&mut self, code: KeyCode) {
        match pressed(code, &[DISCARD, APPLY, SCROLL, TOGGLE]) {
            Some(DISCARD) => {
                self.config.servers = std::mem::take(&mut self.servers_before);
                self.popup = Popup::None;
                self.notice = "Server selection unchanged.".into();
            }
            Some(APPLY) => {
                self.popup = Popup::None;
                self.notice = "Checking the selected servers…".into();
                self.recheck_soon();
            }
            Some(SCROLL) => move_selection(self.snapshot.servers.len(), &mut self.servers, back(code)),
            Some(TOGGLE) => self.toggle_server(),
            _ => {}
        }
    }
    /// The run and setup views; a changed path setting is checked again once edits settle.
    fn main_key(&mut self, code: KeyCode, commands: &mpsc::Sender<Command>) {
        let before = self.config.clone();
        match pressed(code, &[LATENCY, MORE]) {
            Some(LATENCY) if self.snapshot.participants.len() > 1 => {
                let ids = &self.snapshot.participants;
                let shown = ids.iter().position(|id| Some(id.as_str()) == self.latency_server());
                let next = &ids[shown.map_or(0, |index| index + 1) % ids.len()];
                self.latency_pick = (Some(next) != self.snapshot.latency_focus.as_ref()).then(|| next.clone());
            }
            // Go's scrolling keys: pages anywhere, lines too in the run view.
            Some(MORE) if code == KeyCode::Home => self.body_scroll.offset = 0,
            Some(MORE) if code == KeyCode::End => self.body_scroll.offset = u16::MAX,
            Some(MORE) => self.body_scroll.key(code, 10),
            _ if self.live => self.run_key(code, commands),
            _ => self.setup_key(code, commands),
        }
        // The chooser's selection is a draft until Enter.
        if self.popup != Popup::Servers {
            self.recheck_if_changed(&before);
        }
    }
    /// Go's handleRunKey.
    fn run_key(&mut self, code: KeyCode, commands: &mpsc::Sender<Command>) {
        let keys: &[Key] = if self.active() {
            &[STOP, DETAILS]
        } else {
            &[RUN_AGAIN, SETUP, DETAILS]
        };
        match pressed(code, keys) {
            Some(DETAILS) => {
                self.popup = Popup::Details;
                self.details_scroll.offset = 0;
            }
            Some(STOP) if self.cancel != CancelState::Requested => self.cancel = CancelState::Confirming,
            Some(RUN_AGAIN) => self.start(commands),
            Some(SETUP) => {
                // The run consumed the checked paths.
                self.recheck_soon();
                self.notice.clear();
                self.body_scroll.offset = 0;
                self.live = false;
                self.rows.select(Some(0));
            }
            Some(_) => {}
            None => self.body_scroll.key(code, 10),
        }
    }
    /// Go's handleSetupKey.
    fn setup_key(&mut self, code: KeyCode, commands: &mpsc::Sender<Command>) {
        let keys = [
            START, ACTIVATE, RECHECK, STOP, ROWS, SERVERS, AVAILABLE, AUTOMATIC, CHANGE, TOGGLE,
        ];
        match pressed(code, &keys) {
            // A path check never holds back a run; the run replaces it.
            Some(START) => self.start(commands),
            Some(ACTIVATE) if self.rows.selected() == Some(0) => self.start(commands),
            Some(ACTIVATE) => self.activate(),
            Some(RECHECK) => {
                self.send(Command::Verify(self.config.clone()), commands);
            }
            Some(STOP) if self.active() => {
                self.send(Command::Cancel, commands);
            }
            Some(STOP) => self.rows.select(Some(0)),
            Some(ROWS) => move_selection(self.fields().len(), &mut self.rows, back(code)),
            Some(SERVERS) => self.open_servers(),
            Some(AVAILABLE) if !self.checking() => self.use_available(),
            Some(AUTOMATIC) => {
                self.config.throughput_origin = None;
                self.config.throughput_protocol = None;
                self.config.throughput_transport = None;
                self.config.latency_origin = None;
                self.config.latency_transport = None;
                self.notice = "Transport paths set to automatic.".into();
            }
            Some(CHANGE) => self.change_field(!back(code)),
            Some(TOGGLE) => self.toggle(),
            _ => {}
        }
    }
    /// A valid setup starts a run; the results it replaces stay until it starts.
    fn start(&mut self, commands: &mpsc::Sender<Command>) {
        if let Err(error) = self.config.validate() {
            self.notice = error.to_string();
        } else if self.send(Command::Run(self.config.clone()), commands) {
            self.previous = (self.live && self.snapshot.started()).then(|| self.snapshot.clone());
            (self.live, self.starting, self.open_chooser) = (true, true, false);
            self.body_scroll.offset = 0;
            self.popup = Popup::None;
            self.notice = "Checking paths before the test. Press esc to stop.".into();
        }
    }
    /// As in Go, only when some but not all checked servers are ready.
    fn use_available(&mut self) {
        let checked = self.snapshot.servers.iter().filter(|server| server.has_check_result());
        let available: Vec<_> = checked
            .clone()
            .filter(|server| server.checked() && server.error.is_none())
            .take(MAX_SELECTED_SERVERS)
            .map(|server| server.id.clone())
            .collect();
        if !available.is_empty() && available.len() < checked.count() {
            self.config.servers = available;
            self.notice = "Using the available servers.".into();
        }
    }
    /// Like the Go client, the chooser opens on a checked catalogue of several servers.
    fn open_servers(&mut self) {
        if self.checking() {
            self.open_chooser = true;
            self.notice = "Test servers open when the path check finishes.".into();
        } else if self.snapshot.servers.is_empty() {
            self.open_chooser = true;
            self.notice = "Loading servers…".into();
            self.recheck_soon();
        } else if self.snapshot.servers.len() == 1 {
            self.notice = "This catalogue offers one server.".into();
        } else {
            // The chooser edits the selection in place; Esc restores this one.
            self.servers_before = self.config.servers.clone();
            if self.config.servers.is_empty() {
                // The checked servers are the catalogue's default selection.
                self.config.servers = self
                    .snapshot
                    .servers
                    .iter()
                    .filter(|server| server.has_check_result())
                    .take(MAX_SELECTED_SERVERS)
                    .map(|server| server.id.clone())
                    .collect();
            }
            self.servers.select(Some(0));
            self.popup = Popup::Servers;
            self.notice = "Space selects up to four servers; Enter applies.".into();
        }
    }
    fn toggle_server(&mut self) {
        let Some(server) = self
            .servers
            .selected()
            .and_then(|index| self.snapshot.servers.get(index))
        else {
            return;
        };
        if let Some(index) = self.config.servers.iter().position(|id| id == &server.id) {
            self.config.servers.remove(index);
        } else if self.config.servers.len() < MAX_SELECTED_SERVERS {
            self.config.servers.push(server.id.clone());
        } else {
            self.notice = "Select at most four servers.".into();
        }
    }
}

/// The keys that move up, back or left.
fn back(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab | KeyCode::Left
    )
}
fn move_selection(length: usize, state: &mut ListState, back: bool) {
    if length == 0 {
        state.select(None);
        return;
    }
    let next =
        (state.selected().unwrap_or(0) as isize + if back { -1 } else { 1 }).rem_euclid(length as isize) as usize;
    state.select(Some(next));
}
fn panel(title: &str, theme: Theme) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border)
        .style(theme.text)
        .title(Span::styled(title, bold(theme.ink)))
}
fn bold(color: ratatui::style::Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(84).min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
pub fn safe_text(value: &str, limit: usize) -> String {
    value.chars().take(limit.min(MAX_TEXT)).map(terminal_char).collect()
}
fn cell_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}
/// The safe text that fits in `columns` cells.
fn safe_text_width(value: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let mut width = 0;
    let fits = |character: &char| {
        width += cell_width(*character);
        width <= columns
    };
    value
        .chars()
        .take(MAX_TEXT)
        .map(terminal_char)
        .take_while(fits)
        .collect()
}
fn rate(value: Option<f64>) -> String {
    value.filter(|value| value.is_finite() && *value >= 0.0).map_or_else(
        || MISSING.into(),
        |value| graphite_meter_core::format::rate(value / 8.0),
    )
}
fn milliseconds(value: Option<f64>) -> String {
    value.filter(|value| value.is_finite() && *value >= 0.0).map_or_else(
        || MISSING.into(),
        |value| format!("{} ms", graphite_meter_core::format::latency_ms(value)),
    )
}

#[cfg(test)]
pub(crate) mod tests;
