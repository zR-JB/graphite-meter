//! Go's `time.Duration` text, as flags, environment variables and usage defaults spell durations.

use std::time::Duration;

const UNITS: [(&str, u64); 8] = [
    ("ns", 1),
    ("us", 1_000),
    ("\u{b5}s", 1_000),
    ("\u{3bc}s", 1_000),
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];
const LIMIT: u64 = 1 << 63;

/// Parses Go's duration syntax, such as `1h30m` or `-1.5s`, into signed nanoseconds as `time.ParseDuration` does.
pub fn parse(text: &str) -> Result<i64, String> {
    let invalid = || format!("time: invalid duration {}", quote(text.as_bytes()));
    let (negative, mut rest) = match text.as_bytes() {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        rest => (false, rest),
    };
    match rest {
        b"0" => return Ok(0),
        [] => return Err(invalid()),
        _ => {}
    }
    let number_byte = |byte: &u8| *byte == b'.' || byte.is_ascii_digit();
    let mut total = 0_u64;
    while let Some(first) = rest.first() {
        if !number_byte(first) {
            return Err(invalid());
        }
        let (whole, after_whole) = leading_int(rest).ok_or_else(invalid)?;
        let (fraction, scale, after_fraction) = match after_whole.strip_prefix(b".") {
            Some(digits) => leading_fraction(digits),
            None => (0, 1.0, after_whole),
        };
        if !rest[..rest.len() - after_fraction.len()].iter().any(u8::is_ascii_digit) {
            return Err(invalid());
        }
        let unit = after_fraction.split(number_byte).next().unwrap_or_default();
        rest = &after_fraction[unit.len()..];
        if unit.is_empty() {
            return Err(format!("time: missing unit in duration {}", quote(text.as_bytes())));
        }
        let Some(&(_, unit_nanos)) = UNITS.iter().find(|(name, _)| name.as_bytes() == unit) else {
            let (unit, text) = (quote(unit), quote(text.as_bytes()));
            return Err(format!("time: unknown unit {unit} in duration {text}"));
        };
        if whole > LIMIT / unit_nanos {
            return Err(invalid());
        }
        // Go multiplies the fraction by `unit / scale` in floating point.
        let nanos = whole * unit_nanos + (fraction as f64 * (unit_nanos as f64 / scale)) as u64;
        let sum = total.checked_add(nanos).filter(|&sum| nanos <= LIMIT && sum <= LIMIT);
        total = sum.ok_or_else(invalid)?;
    }
    match negative {
        true => Ok((total as i64).wrapping_neg()),
        false => i64::try_from(total).map_err(|_| invalid()),
    }
}

/// The leading digits as a number of at most 2^63.
fn leading_int(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let (digits, rest) = bytes.split_at(bytes.iter().take_while(|byte| byte.is_ascii_digit()).count());
    let value = digits.iter().try_fold(0, |value, &digit| append(value, digit))?;
    Some((value, rest))
}

/// The leading digits as a numerator and its power-of-ten scale; digits past 2^63 are read and dropped.
fn leading_fraction(bytes: &[u8]) -> (u64, f64, &[u8]) {
    let (digits, rest) = bytes.split_at(bytes.iter().take_while(|byte| byte.is_ascii_digit()).count());
    let (mut value, mut scale) = (0, 1.0);
    for &digit in digits {
        let Some(next) = append(value, digit) else { break };
        (value, scale) = (next, scale * 10.0);
    }
    (value, scale, rest)
}

/// `value` with a decimal digit appended, while it stays at most 2^63.
fn append(value: u64, digit: u8) -> Option<u64> {
    value
        .checked_mul(10)?
        .checked_add(u64::from(digit - b'0'))
        .filter(|&value| value <= LIMIT)
}

/// Go's `time` quoting: bytes outside printable ASCII as `\x` escapes.
fn quote(text: &[u8]) -> String {
    let mut quoted = String::from('"');
    for &byte in text {
        match byte {
            b'"' | b'\\' => quoted.extend(['\\', char::from(byte)]),
            b' '..=0x7f => quoted.push(char::from(byte)),
            _ => quoted.push_str(&format!("\\x{byte:02x}")),
        }
    }
    quoted.push('"');
    quoted
}

/// Formats a duration as Go's `Duration.String`, such as `5m0s` or `1.5ms`.
pub fn format(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    match nanos {
        0 => "0s".into(),
        1..1_000 => format!("{nanos}ns"),
        1_000..1_000_000 => format!("{}\u{b5}s", decimal(nanos, 1_000)),
        1_000_000..1_000_000_000 => format!("{}ms", decimal(nanos, 1_000_000)),
        _ => {
            let (hours, minutes) = (nanos / 3_600_000_000_000, nanos / 60_000_000_000 % 60);
            let seconds = decimal(nanos % 60_000_000_000, 1_000_000_000);
            match (hours, minutes) {
                (0, 0) => format!("{seconds}s"),
                (0, _) => format!("{minutes}m{seconds}s"),
                _ => format!("{hours}h{minutes}m{seconds}s"),
            }
        }
    }
}

/// `value / scale` in decimal, without trailing zeros in its fraction.
fn decimal(value: u128, scale: u128) -> String {
    let (whole, fraction) = (value / scale, value % scale);
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = scale.ilog10() as usize;
    format!("{whole}.{}", format!("{fraction:0digits$}").trim_end_matches('0'))
}
