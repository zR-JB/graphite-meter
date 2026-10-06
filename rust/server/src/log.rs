//! Log lines with a time, level and topic, and limits for lines peers or load can repeat.
//!
//! A message states what happened, then after `: ` its detail, then after `; ` what to do or what happens next:
//! `certificate renewal rejected: <error>; keeping the current certificate`. It starts lowercase, has no full stop and
//! uses one word for one meaning: `failed` (an operation did not complete), `refused` (the server declined),
//! `unavailable` (a dependency did not answer), `ready`.

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
        $crate::log::write($crate::log::Level::$level, $topic, format_args!($($message)*))
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

    /// The label's terminal colour: blue, green, bold yellow, bold red.
    fn colour(self) -> &'static str {
        match self {
            Self::Debug => "34",
            Self::Info => "32",
            Self::Warn => "1;33",
            Self::Error => "1;31",
        }
    }
}

/// Colour only on a terminal, and never with a non-empty `NO_COLOR`.
static COLOUR: LazyLock<bool> = LazyLock::new(|| {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
});

pub fn write(level: Level, topic: &str, message: fmt::Arguments<'_>) {
    let line = line(level, topic, message, SystemTime::now(), *COLOUR);
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

/// `2026-10-06T14:31:02Z WARN  tls       message`, its control characters escaped so no peer text drives a terminal.
pub fn line(level: Level, topic: &str, message: fmt::Arguments<'_>, time: SystemTime, colour: bool) -> String {
    let mut text = String::new();
    for character in message.to_string().chars() {
        if character.is_control() {
            text.extend(character.escape_debug());
        } else {
            text.push(character);
        }
    }
    let (time, label) = (rfc3339(time), level.label());
    let mut line = String::new();
    if colour {
        let message = if level == Level::Error { format!("\x1b[31m{text}\x1b[0m") } else { text };
        let colour = level.colour();
        let _ = writeln!(
            line,
            "\x1b[2m{time}\x1b[0m \x1b[{colour}m{label:<5}\x1b[0m \x1b[36m{topic:<9}\x1b[0m {message}"
        );
    } else {
        let _ = writeln!(line, "{time} {label:<5} {topic:<9} {text}");
    }
    line
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
            Some(0) => write(self.level, self.topic, message),
            Some(held) => write(
                self.level,
                self.topic,
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
    fn lines_align_level_and_topic_and_escape_control_characters() {
        let time = UNIX_EPOCH + Duration::from_secs(1_759_761_062);
        let plain = line(Level::Warn, "tls", format_args!("key\u{1b}[31m\nnext"), time, false);
        assert_eq!(plain, "2025-10-06T14:31:02Z WARN  tls       key\\u{1b}[31m\\nnext\n");
        let error = line(Level::Error, "config", format_args!("bad"), time, true);
        assert_eq!(
            error,
            "\x1b[2m2025-10-06T14:31:02Z\x1b[0m \x1b[1;31mERROR\x1b[0m \x1b[36mconfig   \x1b[0m \x1b[31mbad\x1b[0m\n"
        );
    }
}
