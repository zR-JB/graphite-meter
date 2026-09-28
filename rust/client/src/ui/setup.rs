//! Setup rows and editing; Config owns validation.
use super::{MAX_TEXT, Ui, cell_width, safe_character};
use crate::{
    Error,
    config::Config,
    model::Stage,
    vocabulary::{self as words, CADENCES, Term},
};
use crossterm::event::KeyCode;
use graphite_meter_core::{
    discovery::{
        LatencyTransport as Latency,
        Protocol::{self, Http1, Http2, Http3, Negotiated},
        ThroughputTransport as Throughput,
    },
    duration::parse_go_duration,
    origin::canonical_origin,
};
use std::{ops::RangeInclusive, time::Duration};

const STAGE: RangeInclusive<Duration> = Duration::from_secs(1)..=Duration::from_secs(300);
const WARMUP: RangeInclusive<Duration> = Duration::ZERO..=Duration::from_secs(4);

/// A setup row: the group heading it opens, its words, and what its keys change.
pub(super) struct Field {
    pub(super) heading: &'static str,
    pub(super) term: Term,
    pub(super) kind: Kind,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Start,
    Advanced,
    Servers,
    Reset,
    Url,
    /// A whole number the editor changes.
    Count(fn(&Config) -> usize, fn(&mut Config) -> &mut usize),
    /// An origin the editor changes; empty is automatic.
    Origin(fn(&Config) -> &Option<String>, fn(&mut Config) -> &mut Option<String>),
    /// A stage's switch and duration.
    Stage(Stage),
    Warmup,
    /// A path choice, named and explained by its term, and a step forward or back.
    Path(fn(&Config) -> Term, fn(&mut Config, bool)),
    Cadence(fn(&Config) -> Duration, fn(&mut Config, bool)),
    Flag(fn(&Config) -> bool, fn(&mut Config)),
}

const fn field(term: Term, kind: Kind) -> Field {
    under("", term, kind)
}
const fn under(heading: &'static str, term: Term, kind: Kind) -> Field {
    Field { heading, term, kind }
}

/// Go's groups: start, connection, stages, then the rows Advanced shows.
pub(super) const FIELDS: [Field; 21] = [
    field(words::START, Kind::Start),
    under("Connections", words::URL, Kind::Url),
    field(words::SERVERS, Kind::Servers),
    field(
        words::THROUGHPUT_TRANSPORT,
        Kind::Path(
            |c| words::throughput_transport(c.throughput_transport),
            |c, forward| c.throughput_transport = cycle(&THROUGHPUT_TRANSPORTS, c.throughput_transport, forward),
        ),
    ),
    field(
        words::PROTOCOL,
        Kind::Path(
            |c| words::protocol(c.throughput_protocol),
            |c, forward| c.throughput_protocol = cycle(&PROTOCOLS, c.throughput_protocol, forward),
        ),
    ),
    field(
        words::LATENCY_TRANSPORT,
        Kind::Path(
            |c| words::latency_transport(c.latency_transport),
            |c, forward| c.latency_transport = cycle(&LATENCY_TRANSPORTS, c.latency_transport, forward),
        ),
    ),
    under("Stages", words::LATENCY, Kind::Stage(Stage::Latency)),
    field(words::DOWNLOAD, Kind::Stage(Stage::Download)),
    field(words::UPLOAD, Kind::Stage(Stage::Upload)),
    field(words::BIDIRECTIONAL, Kind::Stage(Stage::Bidirectional)),
    field(
        words::LOADED_LATENCY,
        Kind::Flag(|c| c.loaded_latency, |c| c.loaded_latency = !c.loaded_latency),
    ),
    field(words::ADVANCED, Kind::Advanced),
    field(words::WARMUP, Kind::Warmup),
    field(
        words::PING_INTERVAL,
        Kind::Cadence(
            |c| c.ping_interval,
            |c, forward| c.ping_interval = next_cadence(c.ping_interval, forward),
        ),
    ),
    field(
        words::LOADED_PING_INTERVAL,
        Kind::Cadence(
            |c| c.loaded_ping_interval,
            |c, forward| c.loaded_ping_interval = next_cadence(c.loaded_ping_interval, forward),
        ),
    ),
    field(words::STREAMS, Kind::Count(|c| c.streams, |c| &mut c.streams)),
    field(
        words::AUTO_STREAMS,
        Kind::Count(|c| c.auto_streams, |c| &mut c.auto_streams),
    ),
    field(
        words::THROUGHPUT_ORIGIN,
        Kind::Origin(|c| &c.throughput_origin, |c| &mut c.throughput_origin),
    ),
    field(
        words::LATENCY_ORIGIN,
        Kind::Origin(|c| &c.latency_origin, |c| &mut c.latency_origin),
    ),
    field(
        words::INSECURE,
        Kind::Flag(|c| c.insecure, |c| c.insecure = !c.insecure),
    ),
    field(words::RESET, Kind::Reset),
];

const THROUGHPUT_TRANSPORTS: [Option<Throughput>; 3] =
    [None, Some(Throughput::FetchStream), Some(Throughput::WebTransport)];
const PROTOCOLS: [Option<Protocol>; 5] = [None, Some(Http1), Some(Http2), Some(Http3), Some(Negotiated)];
const LATENCY_TRANSPORTS: [Option<Latency>; 3] = [None, Some(Latency::WebSocket), Some(Latency::WebTransport)];

/// The choice after `current`, or before it: backwards goes round the others, so a value
/// the list lacks moves as that many steps forward would move it.
fn cycle<T: Copy + PartialEq>(choices: &[T], mut current: T, forward: bool) -> T {
    for _ in 0..if forward { 1 } else { choices.len() - 1 } {
        let at = choices.iter().position(|choice| *choice == current);
        current = choices[at.map_or(0, |index| (index + 1) % choices.len())];
    }
    current
}

fn next_cadence(interval: Duration, forward: bool) -> Duration {
    cycle(&CADENCES.map(|(.., preset)| preset), interval, forward)
}

impl Ui {
    pub(super) fn fields(&self) -> &'static [Field] {
        let advanced = FIELDS.iter().position(|field| matches!(field.kind, Kind::Advanced));
        &FIELDS[..advanced.filter(|_| !self.advanced).map_or(FIELDS.len(), |row| row + 1)]
    }
    /// The row the cursor is on.
    pub(super) fn field(&self) -> &'static Field {
        &self.fields()[self.rows.selected().unwrap_or(0)]
    }
    /// Left and Right: a duration moves by its unit, a choice steps, and other rows act as Enter.
    pub(super) fn change_field(&mut self, direction: isize) {
        let forward = direction > 0;
        let (duration, unit, bounds) = match self.field().kind {
            Kind::Stage(stage) => (stage_duration(&mut self.config, stage), Duration::from_secs(1), STAGE),
            Kind::Warmup => (&mut self.config.warmup, Duration::from_millis(100), WARMUP),
            Kind::Path(_, step) | Kind::Cadence(_, step) => return step(&mut self.config, forward),
            _ => return self.activate(),
        };
        let moved = if forward {
            duration.saturating_add(unit)
        } else {
            duration.saturating_sub(unit)
        };
        *duration = moved.clamp(*bounds.start(), *bounds.end());
    }
    /// Space turns a stage on or off and otherwise acts like Enter, as in Go.
    pub(super) fn toggle(&mut self) {
        let Kind::Stage(stage) = self.field().kind else {
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
    pub(super) fn explanation(&self, config: &Config) -> &'static str {
        match self.kind {
            Kind::Path(term, _) => term(config).explanation,
            _ => self.term.explanation,
        }
    }
    /// Footer keys for this row, before those every row shares.
    pub(super) fn hints(&self) -> &'static [&'static str] {
        match self.kind {
            Kind::Stage(_) => &["Space toggle", "←/→ 1 s", "Enter edit"],
            Kind::Flag(..) => &["Space toggle", "Tab focus"],
            Kind::Servers => &["Enter choose servers", "Tab focus"],
            Kind::Advanced => &["Enter show/hide", "Tab focus"],
            Kind::Url | Kind::Count(..) | Kind::Origin(..) => &["Enter edit", "Tab focus"],
            Kind::Warmup => &["←/→ 0.1 s", "Enter edit"],
            Kind::Reset => &["Enter reset", "Tab focus"],
            _ => &["←/→ choose", "Tab focus"],
        }
    }
    pub(super) fn value(&self, config: &Config) -> String {
        match self.kind {
            Kind::Start | Kind::Advanced | Kind::Reset => String::new(),
            Kind::Url => config.url.clone(),
            Kind::Servers => config.servers.join(","),
            Kind::Count(count, _) => count(config).to_string(),
            Kind::Origin(origin, _) => origin(config).clone().unwrap_or_default(),
            Kind::Stage(stage) => format!(
                "{} · {} s",
                on_off(config.stages.contains(&stage)),
                seconds(config.duration(stage))
            ),
            Kind::Warmup => seconds(config.warmup),
            Kind::Path(term, _) => term(config).label.into(),
            Kind::Cadence(interval, _) => cadence(interval(config)),
            Kind::Flag(on, _) => on_off(on(config)).into(),
        }
    }
}
fn cadence(interval: Duration) -> String {
    words::cadence(interval).map_or_else(|| format!("Custom ({} ms)", interval.as_millis()), Into::into)
}
pub(super) fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}
pub(super) fn seconds(value: Duration) -> String {
    value.as_secs_f64().to_string()
}

pub(super) struct Edit {
    pub(super) field: &'static Field,
    pub(super) chars: Vec<char>,
    cursor: usize,
}
impl Edit {
    pub(super) fn new(field: &'static Field, value: String) -> Self {
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
        let field = self.field();
        match field.kind {
            Kind::Start => {}
            Kind::Advanced => self.advanced = !self.advanced,
            Kind::Servers => self.open_servers(),
            Kind::Reset if !self.reset_prompt => {
                self.reset_prompt = true;
                self.notice = "Press Enter again to reset every setting; any other key keeps them.".into();
            }
            Kind::Reset => {
                self.reset_prompt = false;
                self.config = Config {
                    url: std::mem::take(&mut self.config.url),
                    servers: std::mem::take(&mut self.config.servers),
                    ..Config::default()
                };
                self.notice = "Settings reset to defaults.".into();
            }
            Kind::Stage(stage) => {
                self.edit = Some(Edit::new(field, format!("{}s", seconds(self.config.duration(stage)))));
            }
            Kind::Warmup => self.edit = Some(Edit::new(field, format!("{}s", seconds(self.config.warmup)))),
            Kind::Url | Kind::Count(..) | Kind::Origin(..) => {
                self.edit = Some(Edit::new(field, field.value(&self.config)));
            }
            Kind::Path(_, step) | Kind::Cadence(_, step) => step(&mut self.config, true),
            Kind::Flag(_, flip) => flip(&mut self.config),
        }
    }
    pub(super) fn apply(&mut self, field: &Field, value: String) -> Result<(), Error> {
        let value = value.trim();
        let mut config = self.config.clone();
        match field.kind {
            Kind::Url => config.url = value.into(),
            Kind::Count(_, count) => *count(&mut config) = value.parse()?,
            // Go reads an empty origin as automatic.
            Kind::Origin(_, origin) => {
                *origin(&mut config) = (!value.is_empty()).then(|| canonical_origin(value)).transpose()?;
            }
            Kind::Warmup => config.warmup = duration(value, "Warmup", WARMUP)?,
            Kind::Stage(stage) => *stage_duration(&mut config, stage) = duration(value, stage.name(), STAGE)?,
            _ => return Err("this field is not editable text".into()),
        }
        config.validate_settings()?;
        if let Kind::Url = field.kind {
            config.servers.clear();
            self.snapshot.servers.clear();
        }
        self.config = config;
        Ok(())
    }
}
