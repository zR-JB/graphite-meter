//! Terminal input owns no measurement IO and never blocks its producer.
mod render;
use crate::{
    Error,
    config::Config,
    model::{Phase, Snapshot, Stage},
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
use graphite_meter_core::discovery::{LatencyTransport, Protocol, ThroughputTransport};
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
                        if let Some(edit) = &mut ui.edit { edit.insert(&text); }
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Field {
    Url,
    Servers,
    ThroughputOrigin,
    Protocol,
    ThroughputTransport,
    LatencyOrigin,
    LatencyTransport,
    LatencyStage,
    DownloadStage,
    UploadStage,
    BidiStage,
    Warmup,
    LatencyDuration,
    DownloadDuration,
    UploadDuration,
    BidiDuration,
    Streams,
    AutoStreams,
    PingInterval,
    LoadedLatency,
    Insecure,
}
const FIELDS: [Field; 21] = [
    Field::Url,
    Field::Servers,
    Field::LatencyStage,
    Field::DownloadStage,
    Field::UploadStage,
    Field::BidiStage,
    Field::Streams,
    Field::AutoStreams,
    Field::LoadedLatency,
    Field::Warmup,
    Field::LatencyDuration,
    Field::DownloadDuration,
    Field::UploadDuration,
    Field::BidiDuration,
    Field::PingInterval,
    Field::ThroughputOrigin,
    Field::Protocol,
    Field::ThroughputTransport,
    Field::LatencyOrigin,
    Field::LatencyTransport,
    Field::Insecure,
];

#[derive(Clone, Copy)]
struct SetupPage {
    label: &'static str,
    start: usize,
    end: usize,
}
impl SetupPage {
    fn fields(self) -> &'static [Field] {
        &FIELDS[self.start..self.end]
    }
}
const PAGES: [SetupPage; 4] = [
    SetupPage {
        label: "Server",
        start: 0,
        end: 2,
    },
    SetupPage {
        label: "Run setup",
        start: 2,
        end: 9,
    },
    SetupPage {
        label: "Timing",
        start: 9,
        end: 15,
    },
    SetupPage {
        label: "Connections",
        start: 15,
        end: 21,
    },
];
impl Field {
    fn label(self) -> &'static str {
        match self {
            Self::Url => "Discovery URL",
            Self::Servers => "Server IDs",
            Self::ThroughputOrigin => "Throughput origin",
            Self::Protocol => "HTTP protocol",
            Self::ThroughputTransport => "Throughput transport",
            Self::LatencyOrigin => "Latency origin",
            Self::LatencyTransport => "Latency transport",
            Self::LatencyStage => "Latency stage",
            Self::DownloadStage => "Download stage",
            Self::UploadStage => "Upload stage",
            Self::BidiStage => "Bidirectional stage",
            Self::Warmup => "Warmup (seconds)",
            Self::LatencyDuration => "Latency (seconds)",
            Self::DownloadDuration => "Download (seconds)",
            Self::UploadDuration => "Upload (seconds)",
            Self::BidiDuration => "Bidirectional (seconds)",
            Self::Streams => "Streams (0 = automatic)",
            Self::AutoStreams => "Automatic stream ceiling",
            Self::PingInterval => "Ping interval (ms)",
            Self::LoadedLatency => "Loaded latency",
            Self::Insecure => "Skip TLS verification",
        }
    }
    fn stage(self) -> Option<Stage> {
        match self {
            Self::LatencyStage => Some(Stage::Latency),
            Self::DownloadStage => Some(Stage::Download),
            Self::UploadStage => Some(Stage::Upload),
            Self::BidiStage => Some(Stage::Bidirectional),
            _ => None,
        }
    }
    fn value(self, config: &Config) -> String {
        if let Some(stage) = self.stage() {
            return on_off(config.stages.contains(&stage)).into();
        }
        match self {
            Self::Url => config.url.clone(),
            Self::Servers => config.servers.join(","),
            Self::ThroughputOrigin => config.throughput_origin.clone().unwrap_or_default(),
            Self::LatencyOrigin => config.latency_origin.clone().unwrap_or_default(),
            Self::Protocol => match config.throughput_protocol {
                None => "automatic",
                Some(Protocol::Http1) => "HTTP/1.1",
                Some(Protocol::Http2) => "HTTP/2",
                Some(Protocol::Http3) => "HTTP/3",
                Some(Protocol::Negotiated) => "negotiated",
            }
            .into(),
            Self::ThroughputTransport => match config.throughput_transport {
                None => "automatic",
                Some(ThroughputTransport::FetchStream) => "HTTP stream",
                Some(ThroughputTransport::WebTransport) => "WebTransport stream",
                Some(ThroughputTransport::WebTransportDatagram) => "WebTransport datagram",
            }
            .into(),
            Self::LatencyTransport => match config.latency_transport {
                None => "automatic",
                Some(LatencyTransport::WebSocket) => "WebSocket",
                Some(LatencyTransport::WebTransport) => "WebTransport",
            }
            .into(),
            Self::Warmup => seconds(config.warmup),
            Self::LatencyDuration => seconds(config.latency_duration),
            Self::DownloadDuration => seconds(config.download_duration),
            Self::UploadDuration => seconds(config.upload_duration),
            Self::BidiDuration => seconds(config.bidirectional_duration),
            Self::Streams => config.streams.to_string(),
            Self::AutoStreams => config.auto_streams.to_string(),
            Self::PingInterval => (config.ping_interval.as_secs_f64() * 1000.0).to_string(),
            Self::LoadedLatency => on_off(config.loaded_latency).into(),
            Self::Insecure => on_off(config.insecure).into(),
            _ => unreachable!("stage field handled above"),
        }
    }
}
fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}
fn seconds(value: Duration) -> String {
    value.as_secs_f64().to_string()
}

struct Edit {
    field: Field,
    chars: Vec<char>,
    cursor: usize,
}
impl Edit {
    fn new(field: Field, value: String) -> Self {
        let chars: Vec<_> = value
            .chars()
            .filter(|c| safe_character(*c))
            .take(MAX_TEXT)
            .collect();
        let cursor = chars.len();
        Self {
            field,
            chars,
            cursor,
        }
    }
    fn insert(&mut self, text: &str) {
        for character in text
            .chars()
            .filter(|c| safe_character(*c))
            .take(MAX_TEXT - self.chars.len())
        {
            self.chars.insert(self.cursor, character);
            self.cursor += 1;
        }
    }
    fn text(&self) -> String {
        self.chars.iter().collect()
    }
    fn viewport(&self, width: usize) -> (String, char, String) {
        let width = width.max(1);
        let mut cursor = self.chars.get(self.cursor).copied().unwrap_or(' ');
        if cell_width(cursor) > width {
            cursor = ' ';
        }
        let available = width.saturating_sub(cell_width(cursor).max(1));
        let mut right_width = 0;
        for &character in self.chars.iter().skip(self.cursor + 1) {
            let next = right_width + cell_width(character);
            if next > available / 3 {
                break;
            }
            right_width = next;
        }
        let mut start = self.cursor;
        let mut left_width = 0;
        while start > 0 {
            let next = left_width + cell_width(self.chars[start - 1]);
            if next > available - right_width {
                break;
            }
            start -= 1;
            left_width = next;
        }
        let after_cursor = self.cursor + usize::from(self.cursor < self.chars.len());
        let mut end = after_cursor;
        let mut remaining = available - left_width;
        while let Some(&character) = self.chars.get(end) {
            let width = cell_width(character);
            if width > remaining {
                break;
            }
            remaining -= width;
            end += 1;
        }
        (
            self.chars[start..self.cursor].iter().collect(),
            cursor,
            self.chars[after_cursor..end].iter().collect(),
        )
    }
    fn key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Char(c) if safe_character(c) => self.insert(&c.to_string()),
            _ => {}
        }
    }
}

struct Ui {
    config: Config,
    snapshot: Snapshot,
    theme: Theme,
    page: usize,
    rows: ListState,
    servers: ListState,
    live: bool,
    chooser: bool,
    details: bool,
    details_scroll: u16,
    help: bool,
    edit: Option<Edit>,
    notice: String,
    awaiting: bool,
    latency_focus: Option<String>,
}
impl Ui {
    fn new(config: Config, snapshot: Snapshot) -> Self {
        let mut rows = ListState::default();
        rows.select(Some(0));
        let mut servers = ListState::default();
        servers.select(Some(0));
        Self {
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
            help: false,
            edit: None,
            notice: String::new(),
            awaiting: false,
            latency_focus: None,
        }
    }
    fn update(&mut self, mut snapshot: Snapshot) {
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
        if snapshot.results.is_empty() || snapshot.auth.is_some() {
            self.details = false;
            self.details_scroll = 0;
        }
        self.snapshot = snapshot;
    }
    fn notice(&self) -> (&str, bool) {
        if !self.notice.is_empty() {
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
    fn send(&mut self, command: Command, commands: &mpsc::Sender<Command>) {
        match commands.try_send(command) {
            Ok(()) => {
                self.notice.clear();
                self.awaiting = true;
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.notice = "Controller is busy; try again.".into()
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.notice = "Controller is unavailable.".into()
            }
        }
    }
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let _ = commands.try_send(Command::Quit);
            return true;
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
        if key.code == KeyCode::Char('o') && self.snapshot.auth.is_some() {
            self.send(Command::OpenBrowser, commands);
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
                    self.send(Command::Run(self.config.clone()), commands);
                    self.live = true;
                    self.details = false;
                }
                Err(error) => self.notice = error.to_string(),
            },
            KeyCode::Char('v') if !self.active() => {
                self.send(Command::Verify(self.config.clone()), commands)
            }
            KeyCode::Esc if self.active() => self.send(Command::Cancel, commands),
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
    fn activate(&mut self) {
        let Some(&field) = self
            .rows
            .selected()
            .and_then(|index| PAGES[self.page].fields().get(index))
        else {
            return;
        };
        if let Some(stage) = field.stage() {
            if self.config.stages.contains(&stage) {
                self.config.stages.retain(|existing| *existing != stage);
            } else {
                self.config.stages.push(stage);
            }
            return;
        }
        match field {
            Field::Protocol => {
                self.config.throughput_protocol = match self.config.throughput_protocol {
                    None => Some(Protocol::Http1),
                    Some(Protocol::Http1) => Some(Protocol::Http2),
                    Some(Protocol::Http2) => Some(Protocol::Http3),
                    Some(Protocol::Http3) => Some(Protocol::Negotiated),
                    Some(Protocol::Negotiated) => None,
                }
            }
            Field::ThroughputTransport => {
                self.config.throughput_transport = match self.config.throughput_transport {
                    None => Some(ThroughputTransport::FetchStream),
                    Some(ThroughputTransport::FetchStream) => {
                        Some(ThroughputTransport::WebTransport)
                    }
                    Some(ThroughputTransport::WebTransport) => {
                        Some(ThroughputTransport::WebTransportDatagram)
                    }
                    Some(ThroughputTransport::WebTransportDatagram) => None,
                }
            }
            Field::LatencyTransport => {
                self.config.latency_transport = match self.config.latency_transport {
                    None => Some(LatencyTransport::WebSocket),
                    Some(LatencyTransport::WebSocket) => Some(LatencyTransport::WebTransport),
                    Some(LatencyTransport::WebTransport) => None,
                }
            }
            Field::LoadedLatency => self.config.loaded_latency = !self.config.loaded_latency,
            Field::Insecure => self.config.insecure = !self.config.insecure,
            _ => self.edit = Some(Edit::new(field, field.value(&self.config))),
        }
    }
    fn apply(&mut self, field: Field, value: String) -> Result<(), Error> {
        let value = value.trim();
        match field {
            Field::Url => {
                graphite_meter_core::origin::canonical_origin(value)?;
                self.config.url = value.into();
                self.config.servers.clear();
                self.snapshot.servers.clear();
            }
            Field::Servers => {
                let ids: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .collect();
                if ids.len() > 4
                    || ids
                        .iter()
                        .enumerate()
                        .any(|(index, id)| ids[..index].contains(id))
                {
                    return Err("select at most four distinct server IDs".into());
                }
                self.config.servers = ids;
            }
            Field::ThroughputOrigin | Field::LatencyOrigin => {
                let origin = if value.is_empty() {
                    None
                } else {
                    Some(graphite_meter_core::origin::canonical_origin(value)?)
                };
                if matches!(field, Field::ThroughputOrigin) {
                    self.config.throughput_origin = origin;
                } else {
                    self.config.latency_origin = origin;
                }
            }
            Field::Streams | Field::AutoStreams => {
                let number: usize = value.parse()?;
                if number > 128 || matches!(field, Field::AutoStreams) && number == 0 {
                    return Err(
                        "stream count must be 1..128; fixed streams also permits 0 for automatic"
                            .into(),
                    );
                }
                if matches!(field, Field::Streams) {
                    self.config.streams = number;
                } else {
                    self.config.auto_streams = number;
                }
            }
            _ => {
                let number: f64 = value.parse()?;
                if !number.is_finite()
                    || number < 0.0
                    || number > 86400.0
                    || number == 0.0 && !matches!(field, Field::Warmup)
                {
                    return Err(
                        "enter a positive duration up to 86400 (warmup also permits zero)".into(),
                    );
                }
                let duration =
                    Duration::try_from_secs_f64(if matches!(field, Field::PingInterval) {
                        number / 1000.0
                    } else {
                        number
                    })?;
                match field {
                    Field::Warmup => self.config.warmup = duration,
                    Field::LatencyDuration => self.config.latency_duration = duration,
                    Field::DownloadDuration => self.config.download_duration = duration,
                    Field::UploadDuration => self.config.upload_duration = duration,
                    Field::BidiDuration => self.config.bidirectional_duration = duration,
                    Field::PingInterval => self.config.ping_interval = duration,
                    _ => return Err("this field is not editable text".into()),
                }
            }
        }
        Ok(())
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
mod tests {
    use super::*;
    #[test]
    fn terminal_text_cannot_emit_controls_or_direction_overrides() {
        let text = safe_text("server\x1b]52;c;secret\x07\r\n\u{202e}name", 100);
        assert!(text.chars().all(safe_character));
        assert!(!text.contains('\x1b'));
        assert_eq!(safe_text("abcdef", 3), "abc");
    }
    #[test]
    fn editing_is_unicode_safe_and_bounded() {
        let mut edit = Edit::new(Field::Url, "a🦀b".into());
        edit.key(KeyCode::Left);
        edit.key(KeyCode::Backspace);
        assert_eq!(edit.text(), "ab");
        edit.insert("\x1b\n界");
        assert_eq!(edit.text(), "a界b");
        edit.insert(&"x".repeat(MAX_TEXT * 2));
        assert_eq!(edit.chars.len(), MAX_TEXT);
    }
    #[test]
    fn editing_keeps_the_whole_value_visible_when_it_fits() {
        use ratatui::{Terminal, backend::TestBackend};

        let url = "https://meter.example/some/moderately/long/path";
        let mut ui = Ui::new(Config::default(), Snapshot::default());
        ui.edit = Some(Edit::new(Field::Url, url.into()));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains(url));

        let edit = Edit::new(Field::Url, "界".repeat(30));
        let (before, cursor, after) = edit.viewport(20);
        assert!(before.width() + cursor.width().unwrap_or(0) + after.width() <= 20);
        assert!(before.ends_with("界"));
    }
    #[test]
    fn sections_cycle_through_setup_and_live_in_both_directions() {
        let mut ui = Ui::new(Config::default(), Snapshot::default());
        for page in 1..PAGES.len() {
            ui.change_section(1);
            assert_eq!(ui.page, page);
            assert!(!ui.live);
        }
        ui.change_section(1);
        assert!(ui.live);
        ui.change_section(1);
        assert_eq!(ui.page, 0);
        assert!(!ui.live);
        ui.change_section(-1);
        assert!(ui.live);
    }
    #[test]
    fn current_setup_notice_is_visible_after_a_failed_run() {
        let mut ui = Ui::new(Config::default(), Snapshot::default());
        let failed = Snapshot {
            error: Some("old transfer error".into()),
            ..Snapshot::default()
        };
        ui.update(failed);
        assert_eq!(ui.notice(), ("old transfer error", true));

        ui.notice = "Transport paths set to automatic.".into();
        assert_eq!(ui.notice(), ("Transport paths set to automatic.", false));

        let next = Snapshot {
            error: Some("new transfer error".into()),
            ..Snapshot::default()
        };
        ui.update(next);
        assert_eq!(ui.notice(), ("new transfer error", true));
    }
    #[test]
    fn details_show_both_server_contributions_and_close_with_escape() {
        use crate::model::{ServerContribution, ServerSummary, StageResult};
        use ratatui::{Terminal, backend::TestBackend};

        let snapshot = Snapshot {
            phase: Phase::Complete,
            results: vec![StageResult {
                stage: Stage::Download,
                elapsed: Duration::from_secs(1),
                down_bytes: 1_500_000,
                up_bytes: 0,
                down_bps: Some(12_000_000.0),
                up_bps: None,
                latency: Default::default(),
                complete: false,
                server_latencies: Vec::new(),
                server_results: vec![
                    ServerContribution {
                        id: "near".into(),
                        down_bytes: 1_500_000,
                        up_bytes: 0,
                        down_bps: Some(12_000_000.0),
                        up_bps: None,
                        error: None,
                    },
                    ServerContribution {
                        id: "far".into(),
                        down_bytes: 0,
                        up_bytes: 0,
                        down_bps: None,
                        up_bps: None,
                        error: Some("peer disconnected".into()),
                    },
                ],
            }],
            servers: vec![
                ServerSummary {
                    id: "near".into(),
                    name: "Near".into(),
                    ..ServerSummary::default()
                },
                ServerSummary {
                    id: "far".into(),
                    name: "Far".into(),
                    ..ServerSummary::default()
                },
            ],
            ..Snapshot::default()
        };
        let mut ui = Ui::new(Config::default(), snapshot);
        ui.live = true;
        let (commands, _) = mpsc::channel(1);
        ui.key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &commands,
        );
        assert!(ui.details);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Near"));
        assert!(rendered.contains("Far"));
        assert!(rendered.contains("peer disconnected"));
        assert!(rendered.contains("12.00 Mbit/s"));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
        assert!(!ui.details);
    }
    #[test]
    fn minimum_supported_terminal_keeps_live_measurement_visible() {
        use crate::model::{Point, ServerLatency, StageResult};
        use ratatui::{Terminal, backend::TestBackend};

        let snapshot = Snapshot {
            phase: Phase::Measuring,
            stage: Some(Stage::Upload),
            latest: Point {
                elapsed: Duration::from_secs(3),
                up_bps: Some(12_000_000.0),
                ..Point::default()
            },
            server_latencies: vec![ServerLatency {
                id: "near".into(),
                latest_ms: Some(25.0),
                ..ServerLatency::default()
            }],
            results: vec![StageResult {
                stage: Stage::Download,
                elapsed: Duration::from_secs(1),
                down_bytes: 1_500_000,
                up_bytes: 0,
                down_bps: Some(12_000_000.0),
                up_bps: None,
                latency: Default::default(),
                complete: true,
                server_latencies: Vec::new(),
                server_results: Vec::new(),
            }],
            ..Snapshot::default()
        };
        let mut ui = Ui::new(Config::default(), snapshot);
        ui.live = true;
        let mut terminal = Terminal::new(TestBackend::new(45, 12)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Upload · 3.0s"));
        assert!(rendered.contains("↑ 12.00 Mbit/s"));
        assert!(rendered.contains("RTT 25.00 ms"));
        assert!(rendered.contains("Download: ↓ 12.00 Mbit/s"));
        assert!(rendered.contains("d details"));

        ui.snapshot.phase = Phase::Complete;
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        let completed = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(completed.contains("r rerun"));
    }
}
