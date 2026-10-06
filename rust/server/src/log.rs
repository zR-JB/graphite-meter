//! Log lines with a time, level, `[gm:topic]` tag and source, and limits for lines peers or load can repeat.
//!
//! A message states what happened, then after `: ` its detail, then after `; ` what to do or what happens next:
//! `certificate renewal rejected: <error>; keeping the current certificate`. It starts lowercase, has no full stop and
//! uses one word for one meaning: `failed` (an operation did not complete), `refused` (the server declined),
//! `unavailable` (a dependency did not answer), `missing`, `ready`.

use std::{
    fmt::{self, Write as _},
    io::{IsTerminal, Write},
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

    /// The syslog priority journald reads from a `<N>` prefix.
    fn priority(self) -> u8 {
        match self {
            Self::Debug => 7,
            Self::Info => 6,
            Self::Warn => 4,
            Self::Error => 3,
        }
    }

    /// The label's and the message's terminal colours.
    fn colours(self) -> (&'static str, &'static str) {
        match self {
            Self::Debug => ("34", "2"),
            Self::Info => ("32", "0"),
            Self::Warn => ("1;33", "33"),
            Self::Error => ("1;31", "1;31"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// `2026-10-06T16:31:02+02:00 WARN  tls:       message; advice (transport::tls)`, for files, pipes and `docker logs`.
    Plain,
    /// The plain line in colour; a warning's or error's advice on a `help:` line.
    Colour,
    /// `<4>tls: message; advice (transport::tls)`: journald adds the time and reads the level from the prefix.
    Journal,
}

/// journald when systemd connected stderr to it, else colour on a terminal that is not `TERM=dumb` unless `NO_COLOR`
/// is set; `FORCE_COLOR` asks for colour anywhere. Windows consoles get colour only when asked.
static STYLE: LazyLock<Style> = LazyLock::new(|| {
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0");
    let terminal =
        cfg!(unix) && std::io::stderr().is_terminal() && std::env::var_os("TERM").is_some_and(|term| term != "dumb");
    match () {
        _ if set("JOURNAL_STREAM") => Style::Journal,
        _ if !set("NO_COLOR") && (set("FORCE_COLOR") || terminal) => Style::Colour,
        _ => Style::Plain,
    }
});

pub fn write(level: Level, topic: &str, module: &str, message: fmt::Arguments<'_>) {
    let line = line(level, topic, module, message, &crate::clock::local(SystemTime::now()), *STYLE);
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

/// Topics are padded to the longest, `discovery:`, so messages start in one column.
const TOPIC_WIDTH: usize = 10;

/// One log line; control characters are escaped so no peer text drives a terminal. Warnings and errors name the
/// module that wrote them, such as `transport::tls`.
pub fn line(level: Level, topic: &str, module: &str, message: fmt::Arguments<'_>, time: &str, style: Style) -> String {
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
    let source = module.map(|module| format!(" ({module})")).unwrap_or_default();
    let (topic, label) = (format!("{topic}:"), level.label());
    match style {
        Style::Plain => format!("{time} {label:<5} {topic:<TOPIC_WIDTH$} {text}{source}\n"),
        Style::Journal => format!("<{}>{topic} {text}{source}\n", level.priority()),
        Style::Colour => {
            let (badge, colour) = level.colours();
            let advice = text
                .split_once("; ")
                .filter(|_| matches!(level, Level::Warn | Level::Error));
            let (what, advice) = advice.map_or((text.as_str(), None), |(what, advice)| (what, Some(advice)));
            let dim = if source.is_empty() { source } else { format!("\x1b[2m{source}\x1b[0m") };
            let mut line = format!(
                "\x1b[2m{time}\x1b[0m \x1b[{badge}m{label:<5}\x1b[0m \x1b[1m{topic:<TOPIC_WIDTH$}\x1b[0m \
                 \x1b[{colour}m{what}\x1b[0m{dim}\n"
            );
            if let Some(advice) = advice {
                let indent = time.len() + 8 + TOPIC_WIDTH - "help: ".len();
                let _ = writeln!(line, "{:indent$}\x1b[1;36mhelp:\x1b[0m {advice}", "");
            }
            line
        }
    }
}

/// A whole-second UTC time as Go's `time.RFC3339` prints it, such as `2026-10-04T23:59:59Z`.
pub fn rfc3339(time: SystemTime) -> String {
    let [year, month, day, hour, minute, second] = utc(time);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// A whole-second UTC time as HTTP dates and cookie expiries print it, such as `Thu, 01 Jan 1970 00:00:01 GMT`.
pub fn http_date(time: SystemTime) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let [year, month, day, hour, minute, second] = utc(time);
    let days = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() / 86_400;
    let (weekday, month) = (WEEKDAYS[(days % 7) as usize], MONTHS[month as usize - 1]);
    format!("{weekday}, {day:02} {month} {year:04} {hour:02}:{minute:02}:{second:02} GMT")
}

fn utc(time: SystemTime) -> [u64; 6] {
    let seconds = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (year, month, day) = civil(seconds / 86_400);
    [year, month, day, seconds / 3600 % 24, seconds / 60 % 60, seconds % 60]
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let (era, day_of_era) = (shifted / 146_097, shifted % 146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    (year_of_era + era * 400 + u64::from(month <= 2), month, day)
}

const INTERVAL: Duration = Duration::from_secs(60);

/// A line any peer can cause: written at most once a minute, the next one counting those held back.
pub struct RateLimited {
    level: Level,
    topic: &'static str,
    /// What the held-back lines report, such as `peer connection failures`.
    what: &'static str,
    state: Mutex<Window>,
}

#[derive(Default)]
struct Window {
    next: Option<Instant>,
    held: u64,
}

impl RateLimited {
    pub const fn new(level: Level, topic: &'static str, what: &'static str) -> Self {
        Self {
            level,
            topic,
            what,
            state: Mutex::new(Window { next: None, held: 0 }),
        }
    }

    pub fn write(&self, message: fmt::Arguments<'_>) {
        match self.admit(Instant::now()) {
            Some(0) => write(self.level, self.topic, "", message),
            Some(held) => write(
                self.level,
                self.topic,
                "",
                format_args!("{message} (and {held} more {} in the last minute)", self.what),
            ),
            None => {}
        }
    }

    /// The lines held back since the last one written, when one may be written at `now`.
    fn admit(&self, now: Instant) -> Option<u64> {
        let mut window = crate::lock(&self.state);
        if window.next.is_some_and(|next| now < next) {
            window.held += 1;
            return None;
        }
        window.next = Some(now + INTERVAL);
        Some(std::mem::take(&mut window.held))
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
        let swap = || self.0.compare_exchange(on, !on, Ordering::Relaxed, Ordering::Relaxed);
        (changed && swap().is_ok()).then_some(!on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_carry_level_topic_and_source_in_each_style_and_escape_control_characters() {
        let time = "2025-10-06T16:31:02+02:00";
        let tls = "graphite_meter_server::transport::tls";
        let warn = |style| line(Level::Warn, "tls", tls, format_args!("key\u{1b}[31m\nnext; renew it"), time, style);
        let plain = "2025-10-06T16:31:02+02:00 WARN  tls:       key\\u{1b}[31m\\nnext; renew it (transport::tls)\n";
        assert_eq!(warn(Style::Plain), plain);
        assert_eq!(warn(Style::Journal), "<4>tls: key\\u{1b}[31m\\nnext; renew it (transport::tls)\n");
        let info = line(Level::Info, "listen", tls, format_args!("ready"), time, Style::Plain);
        assert_eq!(
            info, "2025-10-06T16:31:02+02:00 INFO  listen:    ready\n",
            "only warnings and errors name a source"
        );
        let error = line(
            Level::Error,
            "config",
            "graphite_meter_server",
            format_args!("bad: x; fix it"),
            time,
            Style::Colour,
        );
        let expected = format!(
            "\x1b[2m2025-10-06T16:31:02+02:00\x1b[0m \x1b[1;31mERROR\x1b[0m \x1b[1mconfig:   \x1b[0m \
             \x1b[1;31mbad: x\x1b[0m\n{:37}\x1b[1;36mhelp:\x1b[0m fix it\n",
            ""
        );
        assert_eq!(error, expected);
    }
}
