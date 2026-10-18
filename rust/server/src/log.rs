//! Log lines with a time, level, topic and source, and limits for lines peers or load can repeat.
//! Messages read `what happened: detail; what next`, lowercase, no full stop, and keep one word per meaning.

use jiff::{Timestamp, tz::TimeZone};
use std::{
    fmt::{self, Write as _},
    io::{IsTerminal, Write},
    sync::{
        LazyLock, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    time::{Duration, Instant, SystemTime},
};

/// Writes one line to stderr: `log!(Warn, "tls", "certificate expires in {left}")`.
#[macro_export]
macro_rules! log {
    ($level:ident, $topic:expr, $($message:tt)*) => {
        $crate::log::write($crate::log::Level::$level, $topic, module_path!(), format_args!($($message)*))
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    /// The label's and the message's terminal colors.
    fn colors(self) -> (&'static str, &'static str) {
        match self {
            Self::Debug => ("34", "2"),
            Self::Info => ("32", "0"),
            Self::Warn => ("1;33", "33"),
            Self::Error => ("1;31", "1;31"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// `2026-10-06T14:31:02Z WARN  tls:       message; advice (transport::tls)`, for files, pipes and `docker logs`.
    Plain,
    /// The plain line in color; a warning's or error's advice on a `help:` line.
    Color,
}

/// Color on a terminal that is not `TERM=dumb` unless `NO_COLOR` is set; `FORCE_COLOR` asks for it anywhere.
static STYLE: LazyLock<Style> = LazyLock::new(|| {
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0");
    let terminal = cfg!(unix)
        && std::io::stderr().is_terminal()
        && std::env::var_os("TERM").is_some_and(|term| term != "dumb");
    if !set("NO_COLOR") && (set("FORCE_COLOR") || terminal) {
        Style::Color
    } else {
        Style::Plain
    }
});

/// Lines waiting for the writer thread; past this many a line is dropped and counted, so a stalled pipe never blocks.
const QUEUE: usize = 1024;
/// How long `flush` waits for the queue to drain.
const FLUSH_WAIT: Duration = Duration::from_secs(2);

enum Entry {
    Line(String),
    /// Answered once every earlier line is written.
    Flush(SyncSender<()>),
}

/// The writer thread's queue; none if the thread could not start, and lines are then written in place.
static WRITER: LazyLock<Option<SyncSender<Entry>>> = LazyLock::new(|| {
    let (queue, entries) = mpsc::sync_channel(QUEUE);
    let writer = std::thread::Builder::new()
        .name("gm-log".into())
        .spawn(move || {
            for entry in entries {
                match entry {
                    Entry::Line(line) => emit(&line),
                    Entry::Flush(done) => drop(done.send(())),
                }
                let dropped = DROPPED.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    let message =
                        format_args!("log lines dropped: {dropped}; stderr was not taking writes");
                    emit(&line(
                        Level::Warn,
                        "log",
                        module_path!(),
                        message,
                        &now(),
                        *STYLE,
                    ));
                }
            }
        });
    writer.ok().map(|_| queue)
});
static DROPPED: AtomicU64 = AtomicU64::new(0);

pub fn write(level: Level, topic: &str, module: &str, message: fmt::Arguments<'_>) {
    let line = line(level, topic, module, message, &now(), *STYLE);
    let Some(writer) = WRITER.as_ref() else {
        return emit(&line);
    };
    match writer.try_send(Entry::Line(line)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(entry)) => {
            if let Entry::Line(line) = entry {
                emit(&line);
            }
        }
    }
}

/// Waits until the lines logged so far are written, at most `FLUSH_WAIT`, so lines logged before an exit survive it.
pub fn flush() {
    let Some(writer) = WRITER.as_ref() else {
        return;
    };
    let (done, written) = mpsc::sync_channel(1);
    let deadline = Instant::now() + FLUSH_WAIT;
    let mut entry = Entry::Flush(done);
    loop {
        match writer.try_send(entry) {
            Ok(()) => {
                let _ = written.recv_timeout(deadline.saturating_duration_since(Instant::now()));
                return;
            }
            Err(TrySendError::Full(again)) if Instant::now() < deadline => {
                entry = again;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
}

fn emit(line: &str) {
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

static ZONE: OnceLock<TimeZone> = OnceLock::new();

pub fn show_in(zone: TimeZone) {
    let _ = ZONE.set(zone);
}

fn now() -> String {
    stamp(SystemTime::now(), ZONE.get())
}

/// RFC 3339 to the second in `zone` with the offset in effect then, or in UTC with `Z` without one or out of its range.
fn stamp(time: SystemTime, zone: Option<&TimeZone>) -> String {
    zone.and_then(|zone| {
        let at = Timestamp::try_from(time).ok()?;
        let (local, offset) = (zone.to_datetime(at), zone.to_offset(at).seconds());
        let (sign, minutes) = (
            if offset < 0 { '-' } else { '+' },
            offset.unsigned_abs() / 60,
        );
        Some(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
            local.year(),
            local.month(),
            local.day(),
            local.hour(),
            local.minute(),
            local.second(),
            minutes / 60,
            minutes % 60
        ))
    })
    .unwrap_or_else(|| crate::clock::rfc3339(time))
}

/// Topics are padded to the longest, `admission:`, so messages start in one column.
const TOPIC_WIDTH: usize = 10;

/// One log line, control characters escaped against peer text; warnings and errors name their module.
fn line(
    level: Level,
    topic: &str,
    module: &str,
    message: fmt::Arguments<'_>,
    time: &str,
    style: Style,
) -> String {
    let mut text = String::new();
    for character in message.to_string().chars() {
        if character.is_control() {
            text.extend(character.escape_debug());
        } else {
            text.push(character);
        }
    }
    let module = module
        .split_once("::")
        .map(|(_, path)| path)
        .filter(|_| matches!(level, Level::Warn | Level::Error));
    let source = module
        .map(|module| format!(" ({module})"))
        .unwrap_or_default();
    let (topic, label) = (format!("{topic}:"), level.label());
    match style {
        Style::Plain => format!("{time} {label:<5} {topic:<TOPIC_WIDTH$} {text}{source}\n"),
        Style::Color => {
            let (badge, color) = level.colors();
            let advice = text
                .split_once("; ")
                .filter(|_| matches!(level, Level::Warn | Level::Error));
            let (what, advice) =
                advice.map_or((text.as_str(), None), |(what, advice)| (what, Some(advice)));
            let dim = if source.is_empty() {
                source
            } else {
                format!("\x1b[2m{source}\x1b[0m")
            };
            let mut line = format!(
                "\x1b[2m{time}\x1b[0m \x1b[{badge}m{label:<5}\x1b[0m \x1b[1m{topic:<TOPIC_WIDTH$}\x1b[0m \
                 \x1b[{color}m{what}\x1b[0m{dim}\n"
            );
            if let Some(advice) = advice {
                let indent = time.len() + 8 + TOPIC_WIDTH - "help: ".len();
                let _ = writeln!(line, "{:indent$}\x1b[1;36mhelp:\x1b[0m {advice}", "");
            }
            line
        }
    }
}

/// A span in prose: `80 ms`, `4 s`, `4.5 s`, `1 min 5 s`, `2 h 5 min`.
pub fn span(span: Duration) -> String {
    let (millis, seconds) = (span.as_millis(), span.as_secs());
    let (minutes, hours) = (seconds / 60 % 60, seconds / 3600);
    let part = |value: u64, unit: &str| {
        if value > 0 {
            format!(" {value} {unit}")
        } else {
            String::new()
        }
    };
    match seconds {
        0 => format!("{millis} ms"),
        1..60 if millis / 100 % 10 > 0 => format!("{seconds}.{} s", millis / 100 % 10),
        1..60 => format!("{seconds} s"),
        60..3600 => format!("{} min{}", seconds / 60, part(seconds % 60, "s")),
        _ => format!("{hours} h{}", part(minutes, "min")),
    }
}

const INTERVAL: Duration = Duration::from_mins(1);

/// A line any peer can cause: written at most once a minute, the next one counting those held back.
pub struct RateLimited {
    level: Level,
    topic: &'static str,
    /// What the held-back lines report, such as `peer connection failures`.
    what: &'static str,
    module: &'static str,
    state: Mutex<Pace>,
}

#[derive(Default)]
struct Pace {
    next: Option<Instant>,
    held: u64,
}

impl Pace {
    /// The lines held back since the last one written, when one may be written at `now`.
    fn admit(&mut self, now: Instant) -> Option<u64> {
        if self.next.is_some_and(|next| now < next) {
            self.held += 1;
            return None;
        }
        self.next = Some(now + INTERVAL);
        Some(std::mem::take(&mut self.held))
    }
}

impl RateLimited {
    /// `module` is the caller's `module_path!()`.
    pub const fn new(
        level: Level,
        topic: &'static str,
        what: &'static str,
        module: &'static str,
    ) -> Self {
        Self {
            level,
            topic,
            what,
            module,
            state: Mutex::new(Pace {
                next: None,
                held: 0,
            }),
        }
    }

    pub fn write(&self, message: fmt::Arguments<'_>) {
        let admitted = crate::lock(&self.state).admit(Instant::now());
        match admitted {
            Some(0) => write(self.level, self.topic, self.module, message),
            Some(held) => write(
                self.level,
                self.topic,
                self.module,
                format_args!(
                    "{message} (and {held} more {} in the last minute)",
                    self.what
                ),
            ),
            None => {}
        }
    }
}

const PEERS: usize = 64;

/// A line each peer can cause, once a minute per peer for up to `PEERS` peers, past them held back uncounted.
pub(crate) struct PerPeer<K>(Mutex<Vec<(K, Pace)>>);

impl<K: PartialEq> PerPeer<K> {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    pub(crate) fn admit(&self, peer: K) -> Option<u64> {
        self.admit_at(peer, Instant::now())
    }

    fn admit_at(&self, peer: K, now: Instant) -> Option<u64> {
        let mut peers = crate::lock(&self.0);
        if let Some((_, pace)) = peers.iter_mut().find(|(known, _)| *known == peer) {
            return pace.admit(now);
        }
        if peers.len() == PEERS {
            peers.retain(|(_, pace)| pace.next.is_some_and(|next| now < next));
        }
        (peers.len() < PEERS).then(|| {
            peers.push((
                peer,
                Pace {
                    next: Some(now + INTERVAL),
                    held: 0,
                },
            ));
            0
        })
    }
}

/// A condition logged at start and end, each at its own threshold so a value hovering at one cannot flood the log.
#[derive(Debug, Default)]
pub struct Latch(AtomicBool);

impl Latch {
    /// `Some(true)` when the condition starts, `Some(false)` when it ends; one caller sees each change.
    pub fn update(&self, start: bool, end: bool) -> Option<bool> {
        let on = self.0.load(Ordering::Relaxed);
        let changed = if on { end } else { start };
        let swap = || {
            self.0
                .compare_exchange(on, !on, Ordering::Relaxed, Ordering::Relaxed)
        };
        (changed && swap().is_ok()).then_some(!on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_level_topic_and_source_in_each_style_and_escape_control_characters() {
        let time = "2025-10-06T14:31:02Z";
        let tls = "graphite_meter_server::transport::tls";
        let warn = |style| {
            line(
                Level::Warn,
                "tls",
                tls,
                format_args!("key\u{1b}[31m\nnext; renew it"),
                time,
                style,
            )
        };
        let plain = "2025-10-06T14:31:02Z WARN  tls:       key\\u{1b}[31m\\nnext; renew it (transport::tls)\n";
        assert_eq!(warn(Style::Plain), plain);
        let info = line(
            Level::Info,
            "listen",
            tls,
            format_args!("ready"),
            time,
            Style::Plain,
        );
        assert_eq!(
            info, "2025-10-06T14:31:02Z INFO  listen:    ready\n",
            "only warnings and errors name a source"
        );
        let error = line(
            Level::Error,
            "config",
            "graphite_meter_server",
            format_args!("bad: x; fix it"),
            time,
            Style::Color,
        );
        let expected = format!(
            "\x1b[2m2025-10-06T14:31:02Z\x1b[0m \x1b[1;31mERROR\x1b[0m \x1b[1mconfig:   \x1b[0m \
             \x1b[1;31mbad: x\x1b[0m\n{:32}\x1b[1;36mhelp:\x1b[0m fix it\n",
            ""
        );
        assert_eq!(error, expected);
    }

    #[test]
    fn each_peer_writes_once_a_minute_counting_the_lines_held_back_and_at_most_sixty_four_are_paced()
     {
        let paced = PerPeer::new();
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        assert_eq!(paced.admit_at(0, at(0)), Some(0));
        assert_eq!(paced.admit_at(0, at(30)), None);
        assert_eq!(
            paced.admit_at(1, at(30)),
            Some(0),
            "each peer has its own minute"
        );
        assert_eq!(paced.admit_at(0, at(59)), None);
        assert_eq!(paced.admit_at(0, at(60)), Some(2));
        for peer in 2..PEERS {
            assert_eq!(paced.admit_at(peer, at(61)), Some(0));
        }
        assert_eq!(
            paced.admit_at(PEERS, at(61)),
            None,
            "no slot while every peer's minute runs"
        );
        assert_eq!(
            paced.admit_at(PEERS, at(90)),
            Some(0),
            "the slot of the peer whose minute ended"
        );
        assert_eq!(crate::lock(&paced.0).len(), PEERS);
    }

    #[test]
    fn times_carry_the_zone_offset_in_effect_and_fall_back_to_utc() {
        let berlin = jiff::tz::TimeZoneDatabase::bundled()
            .get("Europe/Berlin")
            .unwrap();
        let utc = |fields| crate::clock::utc(fields).unwrap();
        for (fields, local) in [
            ([2026, 1, 15, 12, 0, 0], "2026-01-15T13:00:00+01:00"),
            ([2026, 7, 15, 12, 0, 0], "2026-07-15T14:00:00+02:00"),
            ([2026, 3, 29, 0, 59, 59], "2026-03-29T01:59:59+01:00"),
            ([2026, 3, 29, 1, 0, 0], "2026-03-29T03:00:00+02:00"),
            ([2026, 10, 25, 0, 59, 59], "2026-10-25T02:59:59+02:00"),
            ([2026, 10, 25, 1, 0, 0], "2026-10-25T02:00:00+01:00"),
        ] {
            assert_eq!(stamp(utc(fields), Some(&berlin)), local);
        }
        assert_eq!(
            stamp(utc([2026, 1, 15, 12, 0, 0]), None),
            "2026-01-15T12:00:00Z"
        );
        let beyond = SystemTime::UNIX_EPOCH + Duration::from_secs(400_000_000_000);
        assert_eq!(stamp(beyond, Some(&berlin)), crate::clock::rfc3339(beyond));
    }
}
