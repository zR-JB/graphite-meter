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
    let seconds = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (year, month, day) = civil(seconds / 86_400);
    let (hour, minute, second) = (seconds / 3600 % 24, seconds / 60 % 60, seconds % 60);
    format!("{year:04}/{month:02}/{day:02} {hour:02}:{minute:02}:{second:02}")
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

/// A condition reported when it starts and when it ends, each at its own threshold so that a value hovering at one
/// cannot flood the log.
#[derive(Debug, Default)]
pub struct Latch(AtomicBool);

impl Latch {
    /// `Some(true)` when the condition starts (`start` holds), `Some(false)` when it ends (`end` holds); one caller
    /// sees each change.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_in_go_log_format() {
        let at = |seconds| timestamp(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0), "1970/01/01 00:00:00");
        assert_eq!(at(951_782_400), "2000/02/29 00:00:00");
        assert_eq!(at(1_791_158_399), "2026/10/04 23:59:59");
        assert_eq!(at(4_107_542_400), "2100/03/01 00:00:00");
    }

    #[test]
    fn a_rate_limited_line_appears_once_a_minute_and_counts_the_rest() {
        let limited = RateLimited::new("peer connection failures");
        let start = Instant::now();
        assert_eq!(limited.admit(start), Some(0));
        assert_eq!(limited.admit(start + Duration::from_secs(1)), None);
        assert_eq!(limited.admit(start + INTERVAL - Duration::from_millis(1)), None);
        assert_eq!(limited.admit(start + INTERVAL), Some(2));
        assert_eq!(limited.admit(start + INTERVAL * 3), Some(0));
    }

    #[test]
    fn a_latch_reports_each_change_once_with_hysteresis() {
        let latch = Latch::default();
        assert_eq!(latch.update(false, true), None, "an ending condition that never started");
        assert_eq!(latch.update(true, false), Some(true));
        assert_eq!(latch.update(true, false), None);
        assert_eq!(latch.update(false, false), None, "between the thresholds");
        assert_eq!(latch.update(false, true), Some(false));
        assert_eq!(latch.update(false, true), None);
    }
}
