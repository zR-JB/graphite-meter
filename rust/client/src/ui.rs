//! Terminal input owns no measurement IO and never blocks its producer.
mod render;
mod setup;
use crate::{
    Error,
    config::Config,
    model::{Phase, Snapshot},
    theme::Theme,
};
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute,
};
use futures_util::StreamExt;
use graphite_meter_core::{catalog::MAX_SELECTED_SERVERS, text::terminal_character as safe_character};
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
use setup::Edit;
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

struct Ui {
    config: Config,
    requested: Config,
    snapshot: Snapshot,
    theme: Theme,
    advanced: bool,
    rows: ListState,
    servers: ListState,
    live: bool,
    /// A requested run that has not started: the check it replaces is not a return to setup.
    starting: bool,
    open_chooser: bool,
    popup: Popup,
    details_scroll: u16,
    auth_scroll: u16,
    help: bool,
    edit: Option<Edit>,
    notice: String,
    recheck: Option<tokio::time::Instant>,
    awaiting: bool,
    cancel: CancelState,
    quitting: bool,
    interrupted: bool,
    latency_pick: Option<String>,
    received_at: tokio::time::Instant,
    shown_down: Option<f64>,
    shown_up: Option<f64>,
}
impl Ui {
    fn new(config: Config, snapshot: Snapshot) -> Self {
        let mut rows = ListState::default();
        rows.select(Some(0));
        let mut servers = ListState::default();
        servers.select(Some(0));
        let (shown_down, shown_up) = (snapshot.latest.down_bps, snapshot.latest.up_bps);
        Self {
            requested: config.clone(),
            config,
            snapshot,
            theme: Theme::terminal(),
            advanced: false,
            rows,
            servers,
            live: false,
            starting: false,
            open_chooser: false,
            popup: Popup::None,
            details_scroll: 0,
            auth_scroll: 0,
            help: false,
            edit: None,
            notice: String::new(),
            recheck: None,
            awaiting: false,
            cancel: CancelState::Idle,
            quitting: false,
            interrupted: false,
            latency_pick: None,
            received_at: tokio::time::Instant::now(),
            shown_down,
            shown_up,
        }
    }
    fn update(&mut self, mut snapshot: Snapshot) {
        if snapshot.auth != self.snapshot.auth {
            self.auth_scroll = 0;
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
            let server = snapshot.servers.iter().find(|server| server.id == failure.server_id);
            let name = server.map_or(failure.server_id.as_str(), |server| server.name.as_str());
            self.notice = format!("{name}: {}", failure.reason.label());
        }
        self.awaiting = false;
        if snapshot.auth.is_some() || !snapshot.phase.live() {
            if self.cancel != CancelState::Idle {
                self.notice.clear();
            }
            self.cancel = CancelState::Idle;
        }
        if snapshot.auth.is_some() || self.popup == Popup::Details && snapshot.results.is_empty() {
            self.popup = Popup::None;
            self.details_scroll = 0;
        }
        if snapshot.stage != self.snapshot.stage {
            self.shown_down = None;
            self.shown_up = None;
        }
        if snapshot.phase == Phase::Checking && !self.starting {
            self.live = false;
        }
        self.starting &= snapshot.phase == Phase::Checking;
        if self
            .latency_pick
            .as_ref()
            .is_some_and(|pick| !snapshot.participants.contains(pick))
        {
            self.latency_pick = None;
        }
        self.received_at = tokio::time::Instant::now();
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
        self.snapshot.latest.elapsed
            + if self.active() {
                self.received_at.elapsed()
            } else {
                Duration::ZERO
            }
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
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.interrupt(commands);
        }
        if self.snapshot.auth.is_some() {
            match key.code {
                KeyCode::Char('q') => return true,
                KeyCode::Char('o') | KeyCode::Enter | KeyCode::Char(' ') => {
                    self.send(Command::OpenBrowser, commands);
                }
                KeyCode::Esc => {
                    if self.send(Command::Cancel, commands) {
                        self.live = false;
                        self.notice = "Sign-in canceled. Press v to request a new code.".into();
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.auth_scroll = self.auth_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.auth_scroll = self.auth_scroll.saturating_add(1);
                }
                KeyCode::PageUp => self.auth_scroll = self.auth_scroll.saturating_sub(4),
                KeyCode::PageDown => self.auth_scroll = self.auth_scroll.saturating_add(4),
                _ => {}
            }
            return false;
        }
        if let Some(edit) = &mut self.edit {
            match key.code {
                KeyCode::Esc => self.edit = None,
                KeyCode::Enter => {
                    let field = edit.field;
                    let value = edit.text();
                    let before = self.config.clone();
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
            return false;
        }
        if key.code == KeyCode::Char('q') {
            return self.quit(commands);
        }
        if self.cancel == CancelState::Confirming {
            self.cancel = CancelState::Idle;
            if key.code == KeyCode::Esc {
                if self.send(Command::Cancel, commands) {
                    self.cancel = CancelState::Requested;
                }
            } else {
                self.notice = "Test continues.".into();
            }
            return false;
        }
        if key.code == KeyCode::Char('?') {
            self.help = !self.help;
            return false;
        }
        if self.popup == Popup::Servers {
            let length = self.snapshot.servers.len();
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.popup = Popup::None,
                KeyCode::Up | KeyCode::Char('k') => move_selection(&mut self.servers, length, -1),
                KeyCode::Down | KeyCode::Char('j') => move_selection(&mut self.servers, length, 1),
                KeyCode::Char(' ') => self.toggle_server(),
                _ => {}
            }
            return false;
        }
        if self.popup == Popup::Details {
            match key.code {
                KeyCode::Esc | KeyCode::Char('d') => self.popup = Popup::None,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.details_scroll = self.details_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.details_scroll = self.details_scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    self.details_scroll = self.details_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.details_scroll = self.details_scroll.saturating_add(10);
                }
                _ => {}
            }
            return false;
        }
        let before = self.config.clone();
        let field_count = self.fields().len();
        match key.code {
            KeyCode::Char('d') if self.live && self.snapshot.auth.is_none() && !self.snapshot.results.is_empty() => {
                self.popup = Popup::Details;
                self.details_scroll = 0;
            }
            KeyCode::Char('l') if self.snapshot.participants.len() > 1 => {
                let ids = &self.snapshot.participants;
                let shown = ids.iter().position(|id| Some(id.as_str()) == self.latency_server());
                let next = &ids[shown.map_or(0, |index| index + 1) % ids.len()];
                self.latency_pick = (Some(next) != self.snapshot.latency_focus.as_ref()).then(|| next.clone());
            }
            // A path check never holds back a run; the run replaces it.
            KeyCode::Char('r') | KeyCode::Enter
                if (!self.live || !self.active())
                    && (key.code == KeyCode::Char('r') || self.live || self.rows.selected() == Some(0)) =>
            {
                match self.config.validate() {
                    Ok(()) => {
                        if self.send(Command::Run(self.config.clone()), commands) {
                            (self.live, self.starting, self.open_chooser) = (true, true, false);
                            self.popup = Popup::None;
                            self.notice = "Checking paths before the test. Press esc to stop.".into();
                        }
                    }
                    Err(error) => self.notice = error.to_string(),
                }
            }
            KeyCode::Char('v') if !self.live => {
                self.send(Command::Verify(self.config.clone()), commands);
            }
            KeyCode::Esc if self.active() && self.live => {
                if self.cancel != CancelState::Requested {
                    self.cancel = CancelState::Confirming;
                }
            }
            KeyCode::Esc if self.active() => {
                self.send(Command::Cancel, commands);
            }
            KeyCode::Esc => {
                if self.live {
                    // The run consumed the checked paths.
                    self.recheck_soon();
                    self.notice.clear();
                }
                self.live = false;
                self.rows.select(Some(0));
            }
            KeyCode::Tab if !self.live => move_selection(&mut self.rows, field_count, 1),
            KeyCode::BackTab if !self.live => move_selection(&mut self.rows, field_count, -1),
            KeyCode::Char('s') if !self.live => self.open_servers(),
            KeyCode::Char('u') if !self.live && !self.checking() => {
                let checked = self.snapshot.servers.iter().filter(|server| server.has_check_result());
                let available: Vec<_> = checked
                    .clone()
                    .filter(|server| server.checked() && server.error.is_none())
                    .take(MAX_SELECTED_SERVERS)
                    .map(|server| server.id.clone())
                    .collect();
                // As in Go, only when some but not all checked servers are ready.
                if !available.is_empty() && available.len() < checked.count() {
                    self.config.servers = available;
                    self.notice = "Using the available servers.".into();
                }
            }
            KeyCode::Char('a') if !self.live => {
                self.config.throughput_origin = None;
                self.config.throughput_protocol = None;
                self.config.throughput_transport = None;
                self.config.latency_origin = None;
                self.config.latency_transport = None;
                self.notice = "Transport paths set to automatic.".into();
            }
            KeyCode::Left if !self.live => self.change_field(-1),
            KeyCode::Right if !self.live => self.change_field(1),
            KeyCode::Up | KeyCode::Char('k') if !self.live => move_selection(&mut self.rows, field_count, -1),
            KeyCode::Down | KeyCode::Char('j') if !self.live => move_selection(&mut self.rows, field_count, 1),
            KeyCode::Enter if !self.live => self.activate(),
            KeyCode::Char(' ') if !self.live => self.toggle(),
            _ => {}
        }
        self.recheck_if_changed(&before);
        false
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

fn move_selection(state: &mut ListState, length: usize, direction: isize) {
    if length == 0 {
        state.select(None);
        return;
    }
    let next = (state.selected().unwrap_or(0) as isize + direction).rem_euclid(length as isize) as usize;
    state.select(Some(next));
}
fn panel(title: &str, theme: Theme) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .style(Style::new().fg(theme.text))
        .title(Span::styled(
            title,
            Style::new().fg(theme.ink).add_modifier(Modifier::BOLD),
        ))
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
    value
        .chars()
        .take(limit.min(MAX_TEXT))
        .map(|c| if safe_character(c) { c } else { '�' })
        .collect()
}
fn cell_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}
fn safe_text_width(value: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let mut text = String::new();
    let mut width = 0;
    for character in value.chars().take(MAX_TEXT) {
        let character = if safe_character(character) { character } else { '�' };
        let next = width + cell_width(character);
        if next > columns {
            break;
        }
        text.push(character);
        width = next;
    }
    text
}
fn rate(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or_else(|| "—".into(), |value| graphite_meter_core::format::rate(value / 8.0))
}
fn milliseconds(value: Option<f64>) -> String {
    value.filter(|value| value.is_finite() && *value >= 0.0).map_or_else(
        || "—".into(),
        |value| format!("{} ms", graphite_meter_core::format::latency_ms(value)),
    )
}

#[cfg(test)]
mod tests;
