//! Go `time.ParseDuration` value semantics, including float64 fractional rounding.
use std::{fmt, time::Duration};

/// Go's text for a duration it refuses, which quotes as Rust's Debug does, where Go escapes non-ASCII bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationError(String);

impl fmt::Display for DurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DurationError {}

/// Parse a signed Go duration into nanoseconds. Accepted inputs, truncation, overflow and the order of its
/// errors follow Go 1.27 `time/format.go`.
pub fn parse_go_duration(input: &str) -> Result<i64, DurationError> {
    const LIMIT: u64 = 1 << 63;
    let invalid = || DurationError(format!("time: invalid duration {input:?}"));
    let mut rest = input.as_bytes();
    let negative = rest.first() == Some(&b'-');
    if matches!(rest.first(), Some(b'-' | b'+')) {
        rest = &rest[1..];
    }
    if rest == b"0" {
        return Ok(0);
    }
    if rest.is_empty() {
        return Err(invalid());
    }
    let mut total = 0_u64;
    while !rest.is_empty() {
        let mut integer = 0_u64;
        let mut before = false;
        while let Some(&digit) = rest.first().filter(|b| b.is_ascii_digit()) {
            before = true;
            integer = integer.saturating_mul(10).saturating_add(u64::from(digit - b'0'));
            if integer > LIMIT {
                return Err(invalid());
            }
            rest = &rest[1..];
        }
        let mut fraction = 0_u64;
        let mut scale = 1_f64;
        let mut after = false;
        if rest.first() == Some(&b'.') {
            rest = &rest[1..];
            let mut overflow = false;
            while let Some(&digit) = rest.first().filter(|b| b.is_ascii_digit()) {
                after = true;
                rest = &rest[1..];
                // Once the fraction would pass 1<<63, later digits are read but dropped.
                let next = fraction.saturating_mul(10).saturating_add(u64::from(digit - b'0'));
                overflow |= next > LIMIT;
                if !overflow {
                    fraction = next;
                    scale *= 10.0;
                }
            }
        }
        if !before && !after {
            return Err(invalid());
        }
        let length = rest
            .iter()
            .position(|b| *b == b'.' || b.is_ascii_digit())
            .unwrap_or(rest.len());
        let unit = match &rest[..length] {
            b"ns" => 1_u64,
            b"us" | b"\xc2\xb5s" | b"\xce\xbcs" => 1_000,
            b"ms" => 1_000_000,
            b"s" => 1_000_000_000,
            b"m" => 60_000_000_000,
            b"h" => 3_600_000_000_000,
            b"" => return Err(DurationError(format!("time: missing unit in duration {input:?}"))),
            unit => {
                let unit = String::from_utf8_lossy(unit);
                return Err(DurationError(format!("time: unknown unit {unit:?} in duration {input:?}")));
            }
        };
        rest = &rest[length..];
        // Preserve Go's operation order: f * (unit / scale), not f / scale * unit.
        let fraction = (fraction as f64 * (unit as f64 / scale)) as u64;
        let nanos = integer.saturating_mul(unit).saturating_add(fraction);
        total = total.saturating_add(nanos);
        if nanos > LIMIT || total > LIMIT {
            return Err(invalid());
        }
    }
    if negative {
        Ok((total as i64).wrapping_neg())
    } else {
        i64::try_from(total).map_err(|_| invalid())
    }
}

/// Go's time.Duration.String for nonnegative durations.
pub fn go_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    let decimal = |value: u128, scale: u128| {
        let fraction = format!("{:0width$}", value % scale, width = scale.ilog10() as usize);
        let number = format!("{}.{fraction}", value / scale);
        number.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    match nanos {
        0 => "0s".into(),
        1..1_000 => format!("{nanos}ns"),
        1_000..1_000_000 => format!("{}µs", decimal(nanos, 1_000)),
        1_000_000..1_000_000_000 => format!("{}ms", decimal(nanos, 1_000_000)),
        3_600_000_000_000.. => format!(
            "{}h{}m{}s",
            nanos / 3_600_000_000_000,
            nanos / 60_000_000_000 % 60,
            decimal(nanos % 60_000_000_000, 1_000_000_000)
        ),
        60_000_000_000.. => format!("{}m{}s", nanos / 60_000_000_000, decimal(nanos % 60_000_000_000, 1_000_000_000)),
        _ => format!("{}s", decimal(nanos, 1_000_000_000)),
    }
}

/// A duration without trailing zero units, as Go's native configuration messages.
pub fn short_duration(duration: Duration) -> String {
    let mut text = go_duration(duration);
    if text.ends_with("m0s") {
        text.truncate(text.len() - 2);
    }
    if text.ends_with("h0m") {
        text.truncate(text.len() - 2);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_127_fixed_vectors() {
        // Fractional edge results were obtained with Go 1.27.1 ParseDuration.
        for (text, expected) in [
            ("0", 0),
            ("+0", 0),
            ("-0", 0),
            ("0ns", 0),
            ("1us", 1000),
            ("1µs", 1000),
            ("1μs", 1000),
            ("1h2m3.004005006s", 3_723_004_005_006),
            ("-.5ms", -500_000),
            ("+1.s", 1_000_000_000),
            ("0.333333333333333333333333h", 1_200_000_000_000),
            ("0.999999999999999999999ns", 1),
            ("0.0000000005s0.0000000005s", 0),
            (".000000000277777777777777777h", 1000),
            (".9223372036854775808123456789h", 3_320_413_933_267),
            ("9223372036854775807ns", i64::MAX),
            ("-9223372036854775808ns", i64::MIN),
            ("2562047h47m16.854775807s", i64::MAX),
            ("-2562047h47m16.854775808s", i64::MIN),
        ] {
            assert_eq!(parse_go_duration(text), Ok(expected), "{text}");
        }
    }

    #[test]
    fn rejects_invalid_syntax_and_overflow() {
        for text in [
            "",
            "+",
            "-",
            "00",
            "0.0",
            "1",
            "1e3s",
            " 1s",
            "1s ",
            ".s",
            "1..0s",
            "1s-1s",
            "1d",
            "1S",
            "1 s",
            "１s",
            "1μ",
            "1\0s",
            "9223372036854775808ns",
            "-9223372036854775809ns",
            "9223372036854775807ns1ns",
            "-9223372036854775808ns1ns",
            "2562047h47m16.854775808s",
            "9223372036854775808h",
        ] {
            assert!(parse_go_duration(text).is_err(), "{text}");
        }
    }
}
