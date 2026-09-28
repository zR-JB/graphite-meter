//! Go's `log` line format, in UTC, and its one-line-a-minute limit for failures any peer can cause.
use std::{
    fmt,
    io::Write,
    sync::{Mutex, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

#[macro_export]
macro_rules! log {
    ($($message:tt)*) => {
        $crate::log::write(format_args!($($message)*))
    };
}

pub fn write(message: fmt::Arguments<'_>) {
    let [year, month, day, hour, minute, second] = utc(SystemTime::now());
    let line = format!("{year:04}/{month:02}/{day:02} {hour:02}:{minute:02}:{second:02} {message}\n");
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

/// Go's peerLog writes one line a minute and counts the rest.
const PEER_LOG_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct PeerLog(Mutex<(Option<Instant>, usize)>);

impl PeerLog {
    pub(crate) fn write(&self, message: fmt::Arguments<'_>) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (next, suppressed) = &mut *state;
        let now = Instant::now();
        if next.is_some_and(|next| now < next) {
            *suppressed += 1;
            return;
        }
        *next = Some(now + PEER_LOG_INTERVAL);
        match std::mem::take(suppressed) {
            0 => write(message),
            more => write(format_args!("{message} ({more} more peer connection failures since)")),
        }
    }
}

pub(crate) fn utc(time: SystemTime) -> [u64; 6] {
    let seconds = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (days, seconds) = (seconds / 86_400, seconds % 86_400);
    let era_day = days + 719_468;
    let (era, day_of_era) = (era_day / 146_097, era_day % 146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    [year, month, day, seconds / 3600, seconds / 60 % 60, seconds % 60]
}
