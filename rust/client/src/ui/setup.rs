//! Setup pages and editing; Config owns validation.
use super::{MAX_TEXT, Ui, cell_width, safe_character};
use crate::{Error, config::Config, model::Stage, vocabulary::CADENCES};
use crossterm::event::KeyCode;
use graphite_meter_core::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    duration::parse_go_duration,
    origin::canonical_origin,
};
use std::{ops::RangeInclusive, time::Duration};

const STAGE: RangeInclusive<Duration> = Duration::from_secs(1)..=Duration::from_secs(300);
const WARMUP: RangeInclusive<Duration> = Duration::ZERO..=Duration::from_secs(4);

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
    LoadedPingInterval,
    LoadedLatency,
    Insecure,
}
const FIELDS: [Field; 20] = [
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
    Field::LoadedPingInterval,
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
            let duration = stage_duration(&mut self.config, stage);
            *duration = if direction > 0 {
                duration.saturating_add(Duration::from_secs(1))
            } else {
                duration.saturating_sub(Duration::from_secs(1))
            }
            .clamp(*STAGE.start(), *STAGE.end());
        } else if field == Field::Warmup {
            self.config.warmup = if direction > 0 {
                self.config.warmup.saturating_add(Duration::from_millis(100))
            } else {
                self.config.warmup.saturating_sub(Duration::from_millis(100))
            }
            .min(*WARMUP.end());
        } else {
            let cycles = if direction > 0 {
                1
            } else {
                match field {
                    Field::Protocol => 4,
                    Field::ThroughputTransport | Field::LatencyTransport => 2,
                    Field::PingInterval | Field::LoadedPingInterval => CADENCES.len() - 1,
                    _ => 1,
                }
            };
            for _ in 0..cycles {
                self.activate();
            }
        }
    }
    /// Space turns a stage on or off and otherwise acts like Enter, as in Go.
    pub(super) fn toggle(&mut self) {
        let field = self.rows.selected().and_then(|index| self.fields().get(index));
        let Some(stage) = field.and_then(|field| field.stage()) else {
            return self.activate();
        };
        if self.config.stages.contains(&stage) {
            self.config.stages.retain(|existing| *existing != stage);
        } else {
            self.config.stages.push(stage);
            self.config.stages.sort_unstable();
        }
    }
}

fn stage_duration(config: &mut Config, stage: Stage) -> &mut Duration {
    match stage {
        Stage::Latency => &mut config.latency_duration,
        Stage::Download => &mut config.download_duration,
        Stage::Upload => &mut config.upload_duration,
        Stage::Bidirectional => &mut config.bidirectional_duration,
    }
}

/// As in the Go client, a bare number is seconds and anything else a Go duration.
fn duration(value: &str, label: &str, bounds: RangeInclusive<Duration>) -> Result<Duration, Error> {
    let nanos = match value.parse::<f64>() {
        Ok(seconds) => parse_go_duration(&format!("{seconds}s")),
        Err(_) => parse_go_duration(value),
    }
    .map_err(|_| "use a duration like 800ms, 4s, or 1m; a bare number is seconds")?;
    u64::try_from(nanos)
        .map(Duration::from_nanos)
        .ok()
        .filter(|duration| bounds.contains(duration))
        .ok_or_else(|| {
            let (start, end) = (seconds(*bounds.start()), seconds(*bounds.end()));
            format!("{label} must be from {start} s to {end} s").into()
        })
}

impl Field {
    pub(super) fn term(self) -> crate::vocabulary::Term {
        match self {
            Self::Start => crate::vocabulary::START,
            Self::Advanced => crate::vocabulary::ADVANCED,
            Self::Url => crate::vocabulary::URL,
            Self::Servers => crate::vocabulary::SERVERS,
            Self::ThroughputOrigin => crate::vocabulary::THROUGHPUT_ORIGIN,
            Self::Protocol => crate::vocabulary::PROTOCOL,
            Self::ThroughputTransport => crate::vocabulary::THROUGHPUT_TRANSPORT,
            Self::LatencyOrigin => crate::vocabulary::LATENCY_ORIGIN,
            Self::LatencyTransport => crate::vocabulary::LATENCY_TRANSPORT,
            Self::LatencyStage => crate::vocabulary::LATENCY,
            Self::DownloadStage => crate::vocabulary::DOWNLOAD,
            Self::UploadStage => crate::vocabulary::UPLOAD,
            Self::BidiStage => crate::vocabulary::BIDIRECTIONAL,
            Self::Warmup => crate::vocabulary::WARMUP,
            Self::Streams => crate::vocabulary::STREAMS,
            Self::AutoStreams => crate::vocabulary::AUTO_STREAMS,
            Self::PingInterval => crate::vocabulary::PING_INTERVAL,
            Self::LoadedPingInterval => crate::vocabulary::LOADED_PING_INTERVAL,
            Self::LoadedLatency => crate::vocabulary::LOADED_LATENCY,
            Self::Insecure => crate::vocabulary::INSECURE,
        }
    }
    pub(super) fn label(self) -> &'static str {
        self.term().label
    }
    pub(super) fn explanation(self, config: &Config) -> &'static str {
        match self {
            Self::Protocol => crate::vocabulary::protocol(config.throughput_protocol).explanation,
            Self::ThroughputTransport => {
                crate::vocabulary::throughput_transport(config.throughput_transport).explanation
            }
            Self::LatencyTransport => crate::vocabulary::latency_transport(config.latency_transport).explanation,
            _ => self.term().explanation,
        }
    }
    /// Footer keys for this row, before those every row shares.
    pub(super) fn hints(self) -> &'static [&'static str] {
        match self {
            Self::LatencyStage | Self::DownloadStage | Self::UploadStage | Self::BidiStage => {
                &["Space toggle", "←/→ 1 s", "Enter edit"]
            }
            Self::LoadedLatency | Self::Insecure => &["Space toggle", "Tab focus"],
            Self::Servers => &["Enter choose servers", "Tab focus"],
            Self::Advanced => &["Enter show/hide", "Tab focus"],
            Self::Url | Self::ThroughputOrigin | Self::LatencyOrigin | Self::Streams | Self::AutoStreams => {
                &["Enter edit", "Tab focus"]
            }
            Self::Warmup => &["←/→ 0.1 s", "Enter edit"],
            _ => &["←/→ choose", "Tab focus"],
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
                seconds(config.duration(stage))
            );
        }
        match self {
            Self::Start | Self::Advanced => String::new(),
            Self::Url => config.url.clone(),
            Self::Servers => config.servers.join(","),
            Self::ThroughputOrigin => config.throughput_origin.clone().unwrap_or_default(),
            Self::LatencyOrigin => config.latency_origin.clone().unwrap_or_default(),
            Self::Protocol => crate::vocabulary::protocol(config.throughput_protocol).label.into(),
            Self::ThroughputTransport => crate::vocabulary::throughput_transport(config.throughput_transport)
                .label
                .into(),
            Self::LatencyTransport => crate::vocabulary::latency_transport(config.latency_transport)
                .label
                .into(),
            Self::Warmup => seconds(config.warmup),
            Self::Streams => config.streams.to_string(),
            Self::AutoStreams => config.auto_streams.to_string(),
            Self::PingInterval => cadence(config.ping_interval),
            Self::LoadedPingInterval => cadence(config.loaded_ping_interval),
            Self::LoadedLatency => on_off(config.loaded_latency).into(),
            Self::Insecure => on_off(config.insecure).into(),
            _ => unreachable!("stage field handled above"),
        }
    }
}
fn cadence(interval: Duration) -> String {
    match CADENCES.iter().find(|(.., preset)| *preset == interval) {
        Some((_, label, _)) => (*label).into(),
        None => format!("Custom ({} ms)", interval.as_millis()),
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
        let chars: Vec<_> = value.chars().filter(|c| safe_character(*c)).take(MAX_TEXT).collect();
        let cursor = chars.len();
        Self { field, chars, cursor }
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
        let Some(&field) = self.rows.selected().and_then(|index| self.fields().get(index)) else {
            return;
        };
        if let Some(stage) = field.stage() {
            self.edit = Some(Edit::new(field, format!("{}s", seconds(self.config.duration(stage)))));
            return;
        }
        match field {
            Field::PingInterval | Field::LoadedPingInterval => {
                let cadence = if field == Field::PingInterval {
                    &mut self.config.ping_interval
                } else {
                    &mut self.config.loaded_ping_interval
                };
                let index = CADENCES.iter().position(|(.., preset)| preset == cadence);
                *cadence = CADENCES[index.map_or(0, |index| (index + 1) % CADENCES.len())].2;
            }
            Field::Start => {}
            Field::Advanced => self.advanced = !self.advanced,
            Field::Servers => self.open_servers(),
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
                    Some(ThroughputTransport::FetchStream) => Some(ThroughputTransport::WebTransport),
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
            Field::Warmup => self.edit = Some(Edit::new(field, format!("{}s", seconds(self.config.warmup)))),
            _ => self.edit = Some(Edit::new(field, field.value(&self.config))),
        }
    }
    pub(super) fn apply(&mut self, field: Field, value: String) -> Result<(), Error> {
        let value = value.trim();
        let origin = || (!value.is_empty()).then(|| canonical_origin(value)).transpose();
        let mut config = self.config.clone();
        match field {
            Field::Url => config.url = value.into(),
            Field::ThroughputOrigin => config.throughput_origin = origin()?,
            Field::LatencyOrigin => config.latency_origin = origin()?,
            Field::Streams => config.streams = value.parse()?,
            Field::AutoStreams => config.auto_streams = value.parse()?,
            Field::Warmup => config.warmup = duration(value, "Warmup", WARMUP)?,
            _ => match field.stage() {
                Some(stage) => *stage_duration(&mut config, stage) = duration(value, stage.name(), STAGE)?,
                None => return Err("this field is not editable text".into()),
            },
        }
        config.validate_settings()?;
        if field == Field::Url {
            config.servers.clear();
            self.snapshot.servers.clear();
        }
        self.config = config;
        Ok(())
    }
}
