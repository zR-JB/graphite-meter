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
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers,
    },
    execute,
};
use futures_util::StreamExt;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Borders, Chart, Clear, Dataset, GraphType, List, ListItem,
        ListState, Paragraph, Row, Table, Tabs, Wrap,
    },
};
use setup::{Edit, PAGES};
use std::{
    io::{self, IsTerminal},
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Debug)]
pub enum Command {
    Run(Config),
    Verify(Config),
    Cancel,
    Quit,
    OpenBrowser,
}

const MAX_TEXT: usize = 4096;
const MAX_POINTS: usize = 300;
const MAX_SERVERS: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CancelState {
    #[default]
    Idle,
    Confirming,
    Requested,
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
    _restore: Restore,
}
impl TerminalSession {
    fn enter() -> Result<Self, Error> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("interactive mode requires a terminal on stdin and stdout".into());
        }
        let restore = Restore;
        let terminal = ratatui::try_init()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        Ok(Self {
            terminal,
            _restore: restore,
        })
    }
}

pub async fn run(
    config: Config,
    mut snapshots: watch::Receiver<Snapshot>,
    commands: mpsc::Sender<Command>,
) -> Result<(), Error> {
    let mut session = TerminalSession::enter()?;
    let mut ui = Ui::new(config, snapshots.borrow_and_update().clone());
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_millis(100));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut snapshot_changed = false;
    loop {
        tokio::select! {
            changed = snapshots.changed() => {
                changed.map_err(|_| "measurement controller stopped")?;
                snapshot_changed = true;
            }
            _ = refresh.tick() => {
                if snapshot_changed {
                    ui.update(snapshots.borrow_and_update().clone());
                    snapshot_changed = false;
                    dirty = true;
                }
                if dirty {
                    session.terminal.draw(|frame| ui.draw(frame))?;
                    dirty = false;
                }
            }
            event = events.next() => {
                let Some(event) = event else { return Err("terminal input closed".into()); };
                match event? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if ui.key(key, &commands) { return Ok(()); }
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
    page: usize,
    rows: ListState,
    servers: ListState,
    live: bool,
    chooser: bool,
    details: bool,
    details_scroll: u16,
    auth_scroll: u16,
    help: bool,
    edit: Option<Edit>,
    notice: String,
    awaiting: bool,
    cancel: CancelState,
    latency_focus: Option<String>,
}
impl Ui {
    fn new(config: Config, snapshot: Snapshot) -> Self {
        let mut rows = ListState::default();
        rows.select(Some(0));
        let mut servers = ListState::default();
        servers.select(Some(0));
        Self {
            requested: config.clone(),
            config,
            snapshot,
            theme: Theme::terminal(),
            page: 0,
            rows,
            servers,
            live: false,
            chooser: false,
            details: false,
            details_scroll: 0,
            auth_scroll: 0,
            help: false,
            edit: None,
            notice: String::new(),
            awaiting: false,
            cancel: CancelState::Idle,
            latency_focus: None,
        }
    }
    fn update(&mut self, mut snapshot: Snapshot) {
        if snapshot.auth != self.snapshot.auth {
            self.auth_scroll = 0;
        }
        if snapshot.error != self.snapshot.error {
            self.notice.clear();
        }
        while snapshot.history.len() > MAX_POINTS {
            snapshot.history.pop_front();
        }
        snapshot.servers.truncate(MAX_SERVERS);
        snapshot.server_latencies.truncate(4);
        for host in &mut snapshot.server_latencies {
            while host.history.len() > MAX_POINTS {
                host.history.pop_front();
            }
        }
        snapshot.results.truncate(16);
        self.awaiting = false;
        if snapshot.auth.is_some()
            || !matches!(
                snapshot.phase,
                Phase::Preparing | Phase::Warmup | Phase::Measuring
            )
        {
            if self.cancel != CancelState::Idle {
                self.notice.clear();
            }
            self.cancel = CancelState::Idle;
        }
        if snapshot.results.is_empty() || snapshot.auth.is_some() {
            self.details = false;
            self.details_scroll = 0;
        }
        self.snapshot = snapshot;
    }
    fn notice(&self) -> (&str, bool) {
        if self.cancel == CancelState::Confirming {
            (
                "Cancel the run? Esc confirms; any other key continues.",
                false,
            )
        } else if self.cancel == CancelState::Requested {
            ("Cancelling run; waiting for owned IO.", false)
        } else if !self.notice.is_empty() {
            (&self.notice, false)
        } else if let Some(error) = self.snapshot.error.as_deref() {
            (error, true)
        } else {
            ("", false)
        }
    }
    fn active(&self) -> bool {
        self.awaiting
            || matches!(
                self.snapshot.phase,
                Phase::Preparing | Phase::Warmup | Phase::Measuring
            )
    }
    fn change_page(&mut self, direction: isize) {
        self.page = (self.page as isize + direction).rem_euclid(PAGES.len() as isize) as usize;
        self.rows.select(Some(0));
    }
    fn change_section(&mut self, direction: isize) {
        let current = if self.live { PAGES.len() } else { self.page };
        let next = (current as isize + direction).rem_euclid((PAGES.len() + 1) as isize) as usize;
        self.live = next == PAGES.len();
        if !self.live {
            self.page = next;
            self.rows.select(Some(0));
        }
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
    fn paste(&mut self, text: &str) {
        if self.snapshot.auth.is_none()
            && let Some(edit) = &mut self.edit
        {
            edit.insert(text);
        }
    }
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let _ = commands.try_send(Command::Quit);
            return true;
        }
        if self.snapshot.auth.is_some() {
            match key.code {
                KeyCode::Char('q') => {
                    let _ = commands.try_send(Command::Quit);
                    return true;
                }
                KeyCode::Char('o') | KeyCode::Enter | KeyCode::Char(' ') => {
                    self.send(Command::OpenBrowser, commands);
                }
                KeyCode::Esc => {
                    self.send(Command::Cancel, commands);
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
                    match self.apply(field, value) {
                        Ok(()) => {
                            self.edit = None;
                            self.notice.clear();
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
            let _ = commands.try_send(Command::Quit);
            return true;
        }
        if self.cancel == CancelState::Confirming {
            self.cancel = CancelState::Idle;
            if key.code == KeyCode::Esc {
                if self.send(Command::Cancel, commands) {
                    self.cancel = CancelState::Requested;
                }
            } else {
                self.notice = "Run continues.".into();
            }
            return false;
        }
        if key.code == KeyCode::Char('?') {
            self.help = !self.help;
            return false;
        }
        if self.help {
            if key.code == KeyCode::Esc {
                self.help = false;
            }
            return false;
        }
        if self.chooser {
            let length = self.snapshot.servers.len();
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.chooser = false,
                KeyCode::Up | KeyCode::Char('k') => move_selection(&mut self.servers, length, -1),
                KeyCode::Down | KeyCode::Char('j') => move_selection(&mut self.servers, length, 1),
                KeyCode::Char(' ') => self.toggle_server(),
                _ => {}
            }
            return false;
        }
        if self.details {
            match key.code {
                KeyCode::Esc | KeyCode::Char('d') => self.details = false,
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
        match key.code {
            KeyCode::Char('d')
                if self.live
                    && self.snapshot.auth.is_none()
                    && !self.snapshot.results.is_empty() =>
            {
                self.details = true;
                self.details_scroll = 0;
            }
            KeyCode::Char('l') if !self.snapshot.server_latencies.is_empty() => {
                let hosts = &self.snapshot.server_latencies;
                let current = hosts
                    .iter()
                    .position(|host| Some(&host.id) == self.latency_focus.as_ref())
                    .unwrap_or(0);
                self.latency_focus = Some(hosts[(current + 1) % hosts.len()].id.clone());
            }
            KeyCode::Char('r') if !self.active() => match self.config.validate() {
                Ok(()) => {
                    if self.send(Command::Run(self.config.clone()), commands) {
                        self.live = true;
                        self.details = false;
                    }
                }
                Err(error) => self.notice = error.to_string(),
            },
            KeyCode::Char('v') if !self.active() => {
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
            KeyCode::Esc => self.live = false,
            KeyCode::Tab => self.change_section(1),
            KeyCode::BackTab => self.change_section(-1),
            KeyCode::Char('s') if !self.active() => {
                self.chooser = true;
                self.notice = "Space selects up to four servers; Enter applies.".into();
            }
            KeyCode::Char('a') if !self.active() => {
                self.config.throughput_origin = None;
                self.config.throughput_protocol = None;
                self.config.throughput_transport = None;
                self.config.latency_origin = None;
                self.config.latency_transport = None;
                self.notice = "Transport paths set to automatic.".into();
            }
            KeyCode::Left if !self.live => self.change_page(-1),
            KeyCode::Right if !self.live => self.change_page(1),
            KeyCode::Up | KeyCode::Char('k') if !self.live => {
                move_selection(&mut self.rows, PAGES[self.page].fields().len(), -1)
            }
            KeyCode::Down | KeyCode::Char('j') if !self.live => {
                move_selection(&mut self.rows, PAGES[self.page].fields().len(), 1)
            }
            KeyCode::Enter | KeyCode::Char(' ') if !self.live && !self.active() => self.activate(),
            _ => {}
        }
        false
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
        } else if self.config.servers.len() < 4 {
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
    let next =
        (state.selected().unwrap_or(0) as isize + direction).rem_euclid(length as isize) as usize;
    state.select(Some(next));
}
fn panel(title: &str, theme: Theme) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .title(Span::styled(
            title,
            Style::new()
                .fg(theme.brand_strong)
                .add_modifier(Modifier::BOLD),
        ))
}
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
fn safe_character(c: char) -> bool {
    !c.is_control()
        && !matches!(c,'\u{061c}'|'\u{200b}'..='\u{200f}'|'\u{2028}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}')
}
fn safe_text(value: &str, limit: usize) -> String {
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
        let character = if safe_character(character) {
            character
        } else {
            '�'
        };
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
    match value.filter(|v| v.is_finite() && *v >= 0.0) {
        Some(value) if value >= 1_000_000_000.0 => format!("{:.2} Gbit/s", value / 1_000_000_000.0),
        Some(value) => format!("{:.2} Mbit/s", value / 1_000_000.0),
        None => "—".into(),
    }
}
fn milliseconds(value: Option<f64>) -> String {
    value
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map_or_else(|| "—".into(), |v| format!("{v:.2} ms"))
}

#[cfg(test)]
mod tests;
