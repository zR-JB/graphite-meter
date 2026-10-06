//! Log lines in Go's `log` format with UTC timestamps, and limits for lines peers or load can repeat.

use std::{
    fmt,
    io::Write,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Writes one line to stderr, as `YYYY/MM/DD HH:MM:SS message` in UTC.
#[macro_export]
macro_rules! log {
    ($($message:tt)*) => {
        $crate::log::write(format_args!($($message)*))
    };
}

pub fn write(message: fmt::Arguments<'_>) {
    let line = format!("{} {message}\n", timestamp(SystemTime::now()));
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

fn timestamp(time: SystemTime) -> String {
    let [year, month, day, hour, minute, second] = utc(time);
    format!("{year:04}/{month:02}/{day:02} {hour:02}:{minute:02}:{second:02}")
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
    pub const fn new(what: &'static str) -> Self {
        Self { what, state: Mutex::new(Window { next: None, held: 0 }) }
    }

    pub fn write(&self, message: fmt::Arguments<'_>) {
        match self.admit(Instant::now()) {
            Some(0) => write(message),
            Some(held) => write(format_args!("{message} ({held} more {} since)", self.what)),
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
        (changed
            && self
                .0
                .compare_exchange(on, !on, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok())
        .then_some(!on)
    }
}
