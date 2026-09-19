//! Terminal presentation owns no measurement IO and never blocks its producer.
use crate::{
    Error,
    config::Config,
    model::{Phase, Snapshot, Stage},
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
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState,
        Paragraph, Row, Table, Wrap,
    },
};
use std::{
    io::{self, IsTerminal},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Debug)]
pub enum Command {
    Run(Config),
    Verify(Config),
    Cancel,
    Quit,
}

const MAX_TEXT: usize = 4096;
const MAX_POINTS: usize = 300;
const MAX_SERVERS: usize = 128;
const ACCENT: Color = Color::Cyan;

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
    Field::ThroughputOrigin,
    Field::Protocol,
    Field::ThroughputTransport,
    Field::LatencyOrigin,
    Field::LatencyTransport,
    Field::LatencyStage,
    Field::DownloadStage,
    Field::UploadStage,
    Field::BidiStage,
    Field::Warmup,
    Field::LatencyDuration,
    Field::DownloadDuration,
    Field::UploadDuration,
    Field::BidiDuration,
    Field::Streams,
    Field::AutoStreams,
    Field::PingInterval,
    Field::LoadedLatency,
    Field::Insecure,
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
    rows: ListState,
    servers: ListState,
    live: bool,
    chooser: bool,
    help: bool,
    edit: Option<Edit>,
    notice: String,
    awaiting: bool,
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
            rows,
            servers,
            live: false,
            chooser: false,
            help: false,
            edit: None,
            notice: String::new(),
            awaiting: false,
        }
    }
    fn update(&mut self, mut snapshot: Snapshot) {
        while snapshot.history.len() > MAX_POINTS {
            snapshot.history.pop_front();
        }
        snapshot.servers.truncate(MAX_SERVERS);
        snapshot.results.truncate(16);
        self.awaiting = false;
        self.snapshot = snapshot;
    }
    fn active(&self) -> bool {
        self.awaiting
            || matches!(
                self.snapshot.phase,
                Phase::Preparing | Phase::Warmup | Phase::Measuring
            )
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
        match key.code {
            KeyCode::Char('r') if !self.active() => match self.config.validate() {
                Ok(()) => {
                    self.send(Command::Run(self.config.clone()), commands);
                    self.live = true;
                }
                Err(error) => self.notice = error.to_string(),
            },
            KeyCode::Char('v') if !self.active() => {
                self.send(Command::Verify(self.config.clone()), commands)
            }
            KeyCode::Esc if self.active() => self.send(Command::Cancel, commands),
            KeyCode::Esc => self.live = false,
            KeyCode::Tab | KeyCode::BackTab => self.live = !self.live,
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
            KeyCode::Up | KeyCode::Char('k') if !self.live => {
                move_selection(&mut self.rows, FIELDS.len(), -1)
            }
            KeyCode::Down | KeyCode::Char('j') if !self.live => {
                move_selection(&mut self.rows, FIELDS.len(), 1)
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
        let field = FIELDS[self.rows.selected().unwrap_or(0)];
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
    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 45 || area.height < 12 {
            frame.render_widget(
                Paragraph::new(
                    "Graphite Meter\nEnlarge terminal to at least 45 × 12.\nq quit · Esc cancel",
                )
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let regions = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(area);
        let status = format!(
            "{:?} · {}",
            self.snapshot.phase,
            safe_text(&self.snapshot.status, 160)
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        "GRAPHITE METER",
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  Rust client · {status}")),
                ]),
                Line::from(safe_text(&self.config.url, 200)),
            ]),
            regions[0],
        );
        if self.live {
            self.draw_live(frame, regions[1]);
        } else {
            self.draw_setup(frame, regions[1]);
        }
        let notice = self.snapshot.error.as_deref().unwrap_or(&self.notice);
        frame.render_widget(Paragraph::new(vec![Line::styled(safe_text(notice,300),Style::new().fg(Color::Yellow)),Line::from("r run/rerun · v verify · s servers · Tab setup/live · Esc cancel/back · ? help · q quit")]).wrap(Wrap {trim:true}),regions[2]);
        if self.chooser {
            self.draw_servers(frame);
        }
        if self.help {
            self.draw_help(frame);
        }
        if let Some(edit) = &self.edit {
            let area = popup(frame.area(), 80, 7);
            frame.render_widget(Clear, area);
            let width = usize::from(area.width.saturating_sub(6)).max(1);
            let start = edit.cursor.saturating_sub(width / 2);
            let before: String = edit
                .chars
                .iter()
                .skip(start)
                .take(edit.cursor - start)
                .collect();
            let cursor = edit.chars.get(edit.cursor).copied().unwrap_or(' ');
            let after: String = edit
                .chars
                .iter()
                .skip(edit.cursor + 1)
                .take(width / 2)
                .collect();
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw(before),
                        Span::styled(
                            cursor.to_string(),
                            Style::new().add_modifier(Modifier::REVERSED),
                        ),
                        Span::raw(after),
                    ]),
                    Line::from("Enter apply · Esc discard · ←/→ Home/End move"),
                    Line::styled(safe_text(&self.notice, 200), Style::new().fg(Color::Yellow)),
                ])
                .block(panel(edit.field.label())),
                area,
            );
        }
    }
    fn draw_setup(&mut self, frame: &mut Frame, area: Rect) {
        let regions = if area.width >= 95 {
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(area)
        } else {
            Layout::horizontal([Constraint::Percentage(100), Constraint::Length(0)]).split(area)
        };
        let items = FIELDS
            .iter()
            .map(|field| {
                let value = field.value(&self.config);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<25} ", field.label())),
                    Span::styled(
                        if value.is_empty() {
                            "automatic / default".into()
                        } else {
                            safe_text(&value, 200)
                        },
                        Style::new().fg(ACCENT),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel("Setup · Enter edit/toggle"))
                .highlight_style(Style::new().bg(Color::DarkGray))
                .highlight_symbol("› "),
            regions[0],
            &mut self.rows,
        );
        if regions[1].width > 0 {
            let stages = self
                .config
                .stages
                .iter()
                .map(|stage| {
                    format!(
                        "{}  {} s",
                        stage.name(),
                        seconds(self.config.duration(*stage))
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let selected = if self.config.servers.is_empty() {
                "Default server".into()
            } else {
                safe_text(&self.config.servers.join(", "), 300)
            };
            let text = format!(
                "{selected}\n\n{stages}\n\nWarmup: {} s per transfer stage\n\nLoaded latency: {}\n\nTLS verification: {}\n\nv discovers and checks available servers.\ns chooses up to four servers.\na restores automatic transport paths.",
                seconds(self.config.warmup),
                on_off(self.config.loaded_latency),
                if self.config.insecure {
                    "DISABLED"
                } else {
                    "enabled"
                }
            );
            frame.render_widget(
                Paragraph::new(text)
                    .block(panel("Run plan"))
                    .wrap(Wrap { trim: true }),
                regions[1],
            );
        }
    }
    fn draw_live(&self, frame: &mut Frame, area: Rect) {
        let regions = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(7),
        ])
        .split(area);
        let metrics = format!(
            "↓ {}   ↑ {}   RTT {}   elapsed {:.1}s",
            rate(self.snapshot.latest.down_bps),
            rate(self.snapshot.latest.up_bps),
            milliseconds(self.snapshot.latest.latency_ms),
            self.snapshot.latest.elapsed.as_secs_f64()
        );
        let stage = self.snapshot.stage.map_or("Waiting", Stage::name);
        frame.render_widget(
            Paragraph::new(metrics)
                .block(panel(stage))
                .style(Style::new().fg(ACCENT)),
            regions[0],
        );
        let points = self
            .snapshot
            .history
            .iter()
            .rev()
            .take(MAX_POINTS)
            .collect::<Vec<_>>();
        let latency = self.snapshot.stage == Some(Stage::Latency);
        let series = |download: bool| {
            points
                .iter()
                .rev()
                .filter_map(|point| {
                    let value = if latency {
                        if download { point.latency_ms } else { None }
                    } else if download {
                        point.down_bps
                    } else {
                        point.up_bps
                    }?;
                    let scale = if latency { 1.0 } else { 1_000_000.0 };
                    (value.is_finite() && value >= 0.0)
                        .then_some((point.elapsed.as_secs_f64(), value / scale))
                })
                .collect::<Vec<_>>()
        };
        let down = series(true);
        let up = series(false);
        let maximum = down
            .iter()
            .chain(&up)
            .map(|point| point.1)
            .fold(1.0_f64, f64::max)
            * 1.1;
        let start = points
            .last()
            .map_or(0.0, |point| point.elapsed.as_secs_f64());
        let end = points
            .first()
            .map_or(start + 1.0, |point| point.elapsed.as_secs_f64())
            .max(start + 1.0);
        let datasets = vec![
            Dataset::default()
                .name(if latency { "RTT ms" } else { "↓ Mbps" })
                .marker(Marker::Braille)
                .graph_type(GraphType::Scatter)
                .style(Style::new().fg(Color::Cyan))
                .data(&down),
            Dataset::default()
                .name(if latency { "" } else { "↑ Mbps" })
                .marker(Marker::Braille)
                .graph_type(GraphType::Scatter)
                .style(Style::new().fg(Color::Magenta))
                .data(&up),
        ];
        // Scatter plots leave missing observations empty; no loss is interpolated.
        frame.render_widget(
            Chart::new(datasets)
                .block(panel(if latency {
                    "Recent observed latency"
                } else {
                    "Recent observed throughput"
                }))
                .x_axis(
                    Axis::default()
                        .bounds([start, end])
                        .labels([format!("{start:.0}s"), format!("{end:.0}s")]),
                )
                .y_axis(
                    Axis::default()
                        .bounds([0.0, maximum])
                        .labels(["0".to_owned(), format!("{maximum:.0}")]),
                ),
            regions[1],
        );
        let rows = self.snapshot.results.iter().take(16).map(|result| {
            Row::new(vec![
                format!(
                    "{}{}",
                    result.stage.name(),
                    if result.complete { "" } else { " (partial)" }
                ),
                rate(result.down_bps),
                rate(result.up_bps),
                milliseconds(
                    result
                        .latency
                        .distribution
                        .map(|d| d.p50 as f64 / 1_000_000.0),
                ),
                result.latency.timeouts.to_string(),
                result.latency.unresolved.to_string(),
            ])
        });
        frame.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Percentage(24),
                    Constraint::Percentage(19),
                    Constraint::Percentage(19),
                    Constraint::Percentage(16),
                    Constraint::Percentage(11),
                    Constraint::Percentage(11),
                ],
            )
            .header(
                Row::new(["Stage", "Down", "Up", "RTT p50", "Timeout", "Pending"])
                    .style(Style::new().fg(ACCENT)),
            )
            .block(panel("Stage results · receiver-accounted upload")),
            regions[2],
        );
    }
    fn draw_servers(&mut self, frame: &mut Frame) {
        let area = popup(frame.area(), 100, 24);
        frame.render_widget(Clear, area);
        let items = self
            .snapshot
            .servers
            .iter()
            .take(MAX_SERVERS)
            .map(|server| {
                let selected = self.config.servers.contains(&server.id);
                let detail = server.error.as_deref().unwrap_or(&server.transport);
                ListItem::new(vec![
                    Line::from(format!(
                        "{} {} · {}",
                        if selected { "[✓]" } else { "[ ]" },
                        safe_text(&server.name, 120),
                        safe_text(&server.id, 120)
                    )),
                    Line::styled(
                        format!(
                            "    {}  {}",
                            safe_text(&server.origin, 160),
                            safe_text(detail, 160)
                        ),
                        Style::new().fg(if server.error.is_some() {
                            Color::Yellow
                        } else {
                            Color::DarkGray
                        }),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            frame.render_widget(Paragraph::new("No catalogue yet. Esc returns to setup; v discovers servers.\nServer IDs can also be entered in setup.").wrap(Wrap{trim:true}).block(panel("Servers")),area);
        } else {
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel("Servers · Space toggle · Enter apply · maximum four"))
                    .highlight_style(Style::new().bg(Color::DarkGray)),
                area,
                &mut self.servers,
            );
        }
    }
    fn draw_help(&self, frame: &mut Frame) {
        let area = popup(frame.area(), 78, 18);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new("SETUP\n↑/↓ or j/k  select setting     Enter/Space  edit or toggle\ns  server chooser             v  verify configuration\na  automatic transport paths  r  start measurement\n\nMEASUREMENT\nEsc  cancel active work        r  rerun after completion\nTab  setup / live view         Esc  return to setup\n\nEDITING\n←/→ Home/End  move cursor      Enter  apply     Esc  discard\nPaste is bounded and terminal controls are removed.\n\nq or Ctrl-C  quit              ? or Esc  close help\nMissing samples remain missing; partial results stay labelled.").block(panel("Keyboard help")).wrap(Wrap{trim:true}),area);
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
fn panel(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(title)
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
}
