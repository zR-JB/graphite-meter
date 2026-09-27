//! Setup pages, editing, and configuration validation.
use super::{MAX_TEXT, Ui, cell_width, safe_character};
use crate::{Error, config::Config, model::Stage};
use crossterm::event::KeyCode;
use graphite_meter_core::discovery::{LatencyTransport, Protocol, ThroughputTransport};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Start,
    Advanced,
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
    Streams,
    AutoStreams,
    PingInterval,
    LoadedLatency,
    Insecure,
}
const FIELDS: [Field; 19] = [
    Field::Start,
    Field::Servers,
    Field::Protocol,
    Field::ThroughputTransport,
    Field::LatencyTransport,
    Field::LatencyStage,
    Field::DownloadStage,
    Field::UploadStage,
    Field::BidiStage,
    Field::Warmup,
    Field::Advanced,
    Field::Url,
    Field::ThroughputOrigin,
    Field::LatencyOrigin,
    Field::Streams,
    Field::AutoStreams,
    Field::PingInterval,
    Field::LoadedLatency,
    Field::Insecure,
];

impl Ui {
    pub(super) fn fields(&self) -> &'static [Field] {
        &FIELDS[..if self.advanced { FIELDS.len() } else { 11 }]
    }
    pub(super) fn change_field(&mut self, direction: isize) {
        let field = self.fields()[self.rows.selected().unwrap_or(0)];
        if let Some(stage) = field.stage() {
            let duration = match stage {
                Stage::Latency => &mut self.config.latency_duration,
                Stage::Download => &mut self.config.download_duration,
                Stage::Upload => &mut self.config.upload_duration,
                Stage::Bidirectional => &mut self.config.bidirectional_duration,
            };
            *duration = if direction > 0 {
                duration.saturating_add(Duration::from_secs(1))
            } else {
                duration.saturating_sub(Duration::from_secs(1))
            }
            .clamp(Duration::from_secs(1), Duration::from_secs(300));
        } else if field == Field::Warmup {
            self.config.warmup = if direction > 0 {
                self.config
                    .warmup
                    .saturating_add(Duration::from_millis(100))
            } else {
                self.config
                    .warmup
                    .saturating_sub(Duration::from_millis(100))
            }
            .min(Duration::from_secs(4));
        } else {
            let cycles = if direction > 0 {
                1
            } else {
                match field {
                    Field::Protocol => 4,
                    Field::ThroughputTransport | Field::LatencyTransport => 2,
                    Field::PingInterval => 3,
                    _ => 1,
                }
            };
            for _ in 0..cycles {
                self.activate();
            }
        }
    }
}

impl Field {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Start => "Start test",
            Self::Advanced => "Advanced",
            Self::Url => "Catalogue URL",
            Self::Servers => "Test servers",
            Self::ThroughputOrigin => "Throughput origin",
            Self::Protocol => "HTTP protocol",
            Self::ThroughputTransport => "Throughput transport",
            Self::LatencyOrigin => "Latency origin",
            Self::LatencyTransport => "Latency transport",
            Self::LatencyStage => "Latency",
            Self::DownloadStage => "Download",
            Self::UploadStage => "Upload",
            Self::BidiStage => "Bidirectional",
            Self::Warmup => "Warmup (seconds)",
            Self::Streams => "Streams (0 = automatic)",
            Self::AutoStreams => "Automatic stream ceiling",
            Self::PingInterval => "Latency cadence",
            Self::LoadedLatency => "Loaded latency",
            Self::Insecure => "Skip TLS verification",
        }
    }
    pub(super) fn stage(self) -> Option<Stage> {
        match self {
            Self::LatencyStage => Some(Stage::Latency),
            Self::DownloadStage => Some(Stage::Download),
            Self::UploadStage => Some(Stage::Upload),
            Self::BidiStage => Some(Stage::Bidirectional),
            _ => None,
        }
    }
    pub(super) fn value(self, config: &Config) -> String {
        if let Some(stage) = self.stage() {
            return format!(
                "{} · {} s",
                on_off(config.stages.contains(&stage)),
                config.duration(stage).as_secs()
            );
        }
        match self {
            Self::Start | Self::Advanced => String::new(),
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
                Some(ThroughputTransport::FetchStream) => "Fetch streams",
                Some(_) => "WebTransport streams",
            }
            .into(),
            Self::LatencyTransport => match config.latency_transport {
                None => "automatic",
                Some(LatencyTransport::WebSocket) => "WebSocket",
                Some(LatencyTransport::WebTransport) => "WebTransport datagrams",
            }
            .into(),
            Self::Warmup => seconds(config.warmup),
            Self::Streams => config.streams.to_string(),
            Self::AutoStreams => config.auto_streams.to_string(),
            Self::PingInterval => match config.ping_interval.as_millis() {
                0 => "Reply-driven".into(),
                80 => "Fast (80 ms)".into(),
                250 => "Medium (250 ms)".into(),
                600 => "Slow (600 ms)".into(),
                _ => format!("Custom ({} ms)", config.ping_interval.as_millis()),
            },
            Self::LoadedLatency => on_off(config.loaded_latency).into(),
            Self::Insecure => on_off(config.insecure).into(),
            _ => unreachable!("stage field handled above"),
        }
    }
}
pub(super) fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}
pub(super) fn seconds(value: Duration) -> String {
    value.as_secs_f64().to_string()
}

pub(super) struct Edit {
    pub(super) field: Field,
    pub(super) chars: Vec<char>,
    cursor: usize,
}
impl Edit {
    pub(super) fn new(field: Field, value: String) -> Self {
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
    pub(super) fn insert(&mut self, text: &str) {
        for character in text
            .chars()
            .filter(|c| safe_character(*c))
            .take(MAX_TEXT - self.chars.len())
        {
            self.chars.insert(self.cursor, character);
            self.cursor += 1;
        }
    }
    pub(super) fn text(&self) -> String {
        self.chars.iter().collect()
    }
    pub(super) fn viewport(&self, width: usize) -> (String, char, String) {
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
    pub(super) fn key(&mut self, key: KeyCode) {
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

impl Ui {
    pub(super) fn activate(&mut self) {
        let Some(&field) = self
            .rows
            .selected()
            .and_then(|index| self.fields().get(index))
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
            Field::PingInterval => {
                self.config.ping_interval = match self.config.ping_interval.as_millis() {
                    0 => Duration::from_millis(80),
                    80 => Duration::from_millis(250),
                    250 => Duration::from_millis(600),
                    _ => Duration::ZERO,
                }
            }
            Field::Start => {}
            Field::Advanced => self.advanced = !self.advanced,
            Field::Servers => self.popup = super::Popup::Servers,
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
                    Some(_) => None,
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
    pub(super) fn apply(&mut self, field: Field, value: String) -> Result<(), Error> {
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
                if number > crate::stream_plan::MAX_STREAMS
                    || matches!(field, Field::AutoStreams) && number == 0
                {
                    return Err(
                        "stream count must be 1..14; fixed streams also permits 0 for automatic"
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
                    Field::PingInterval => self.config.ping_interval = duration,
                    _ => return Err("this field is not editable text".into()),
                }
            }
        }
        Ok(())
    }
}
