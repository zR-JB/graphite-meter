//! UTC calendar time to the second: RFC 3339 for logs and documents, HTTP dates for cookies, certificate dates.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Such as `2026-10-06T14:31:02Z`.
pub fn rfc3339(time: SystemTime) -> String {
    let [year, month, day, hour, minute, second] = fields(time);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Such as `Tue, 06 Oct 2026 14:31:02 GMT`.
pub fn http_date(time: SystemTime) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let [year, month, day, hour, minute, second] = fields(time);
    let weekday = WEEKDAYS[(seconds(time) / 86_400 % 7) as usize];
    let month = MONTHS[month as usize - 1];
    format!("{weekday}, {day:02} {month} {year:04} {hour:02}:{minute:02}:{second:02} GMT")
}

/// The time of a proleptic Gregorian date and time of day; `None` for an impossible one or one before 1970.
pub fn utc([year, month, day, hour, minute, second]: [u64; 6]) -> Option<SystemTime> {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 => 28 + u64::from(leap),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    let valid = (1..=12).contains(&month) && (1..=month_days).contains(&day);
    if !valid || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let year = year.checked_sub(u64::from(month <= 2))?;
    let year_of_era = year % 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = (year / 400) * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let seconds = days.checked_sub(719_468)? * 86_400 + hour * 3600 + minute * 60 + second;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn fields(time: SystemTime) -> [u64; 6] {
    let seconds = seconds(time);
    let shifted = seconds / 86_400 + 719_468;
    let (era, day_of_era) = (shifted / 146_097, shifted % 146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    [year, month, day, seconds / 3600 % 24, seconds / 60 % 60, seconds % 60]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_and_impossible_ones_are_refused() {
        let at = |fields| utc(fields).map(rfc3339);
        assert_eq!(at([2026, 10, 4, 23, 59, 59]).as_deref(), Some("2026-10-04T23:59:59Z"));
        assert_eq!(at([2000, 2, 29, 12, 0, 0]).as_deref(), Some("2000-02-29T12:00:00Z"));
        assert_eq!(http_date(utc([2026, 10, 6, 14, 31, 2]).unwrap()), "Tue, 06 Oct 2026 14:31:02 GMT");
        for impossible in [
            [2100, 2, 29, 0, 0, 0],
            [2026, 13, 4, 0, 0, 0],
            [2026, 10, 4, 24, 0, 0],
            [1969, 12, 31, 0, 0, 0],
        ] {
            assert_eq!(utc(impossible), None, "{impossible:?}");
        }
    }
}
