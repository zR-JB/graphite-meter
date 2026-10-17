//! Parses durations as flags and environment variables spell them, such as `800ms`, `10s`, `1m30s` or `1.5h`.

use std::time::Duration;

const UNITS: [(&str, u128); 6] = [
    ("ns", 1),
    ("us", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];
/// Fraction digits past this many stay below a nanosecond in every unit.
const FRACTION_DIGITS: usize = 20;
const INVALID: &str = "use a duration such as 800ms, 10s or 1m30s";
const TOO_LONG: &str = "duration too long";

/// Parses `0` or numbers with units, such as `1h30m` or `1.5s`. Fractions are exact to the nanosecond.
pub fn parse(text: &str) -> Result<Duration, &'static str> {
    match text {
        "0" => return Ok(Duration::ZERO),
        "" => return Err(INVALID),
        _ if text.starts_with('-') => return Err("a duration cannot be negative"),
        _ => {}
    }
    let (mut rest, mut nanos) = (text.as_bytes(), 0_u128);
    while !rest.is_empty() {
        let (whole, after) = digits(rest);
        let (fraction, after) = after.strip_prefix(b".").map_or((&b""[..], after), digits);
        let (unit, after) = after.split_at(after.iter().take_while(|byte| byte.is_ascii_alphabetic()).count());
        let unit = UNITS
            .iter()
            .find(|(name, _)| name.as_bytes() == unit)
            .map(|&(_, nanos)| nanos);
        let Some(unit) = unit.filter(|_| !whole.is_empty() || !fraction.is_empty()) else {
            return Err(INVALID);
        };
        let fraction = &fraction[..fraction.len().min(FRACTION_DIGITS)];
        let fraction_nanos = decimal(fraction).ok_or(TOO_LONG)? * unit / 10_u128.pow(fraction.len() as u32);
        nanos = decimal(whole)
            .and_then(|whole| whole.checked_mul(unit)?.checked_add(fraction_nanos)?.checked_add(nanos))
            .ok_or(TOO_LONG)?;
        rest = after;
    }
    u64::try_from(nanos).map(Duration::from_nanos).map_err(|_| TOO_LONG)
}

fn digits(bytes: &[u8]) -> (&[u8], &[u8]) {
    bytes.split_at(bytes.iter().take_while(|byte| byte.is_ascii_digit()).count())
}

fn decimal(digits: &[u8]) -> Option<u128> {
    digits
        .iter()
        .try_fold(0_u128, |value, &digit| value.checked_mul(10)?.checked_add(u128::from(digit - b'0')))
}

/// Formats a duration as `parse` reads it, such as `5m`, `1m30s`, `1.5s` or `800ms`.
pub fn format(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    match nanos {
        0 => "0s".into(),
        1..1_000 => format!("{nanos}ns"),
        1_000..1_000_000 => format!("{}us", fixed(nanos, 1_000)),
        1_000_000..1_000_000_000 => format!("{}ms", fixed(nanos, 1_000_000)),
        _ => {
            let (hours, minutes, seconds) =
                (nanos / 3_600_000_000_000, nanos / 60_000_000_000 % 60, nanos % 60_000_000_000);
            let mut text = String::new();
            if hours > 0 {
                text += &format!("{hours}h");
            }
            if minutes > 0 {
                text += &format!("{minutes}m");
            }
            if seconds > 0 {
                text += &format!("{}s", fixed(seconds, 1_000_000_000));
            }
            text
        }
    }
}

/// Writes `value / scale` in decimal, without trailing zeros in its fraction.
fn fixed(value: u128, scale: u128) -> String {
    let (whole, fraction) = (value / scale, value % scale);
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = scale.ilog10() as usize;
    format!("{whole}.{}", format!("{fraction:0digits$}").trim_end_matches('0'))
}
