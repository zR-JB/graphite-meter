//! Go `time.ParseDuration` value semantics, including float64 fractional rounding.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationError;

impl fmt::Display for DurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid Go duration")
    }
}
impl std::error::Error for DurationError {}

/// Parse a signed Go duration into nanoseconds. Error wording is intentionally
/// local; accepted inputs, truncation and overflow follow Go 1.27 `time/format.go`.
pub fn parse_go_duration(input: &str) -> Result<i64, DurationError> {
    const LIMIT: u64 = 1 << 63;
    let mut rest = input.as_bytes();
    let negative = rest.first() == Some(&b'-');
    if matches!(rest.first(), Some(b'-' | b'+')) {
        rest = &rest[1..];
    }
    if rest == b"0" {
        return Ok(0);
    }
    if rest.is_empty() {
        return Err(DurationError);
    }
    let mut total = 0_u64;
    while !rest.is_empty() {
        let mut integer = 0_u64;
        let mut before = false;
        while let Some(&digit) = rest.first().filter(|b| b.is_ascii_digit()) {
            before = true;
            if integer > LIMIT / 10 {
                return Err(DurationError);
            }
            integer = integer * 10 + u64::from(digit - b'0');
            if integer > LIMIT {
                return Err(DurationError);
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
                if overflow {
                    continue;
                }
                if fraction > (LIMIT - 1) / 10 {
                    overflow = true;
                    continue;
                }
                let next = fraction * 10 + u64::from(digit - b'0');
                if next > LIMIT {
                    overflow = true;
                    continue;
                }
                fraction = next;
                scale *= 10.0;
            }
        }
        if !before && !after {
            return Err(DurationError);
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
            _ => return Err(DurationError),
        };
        rest = &rest[length..];
        if integer > LIMIT / unit {
            return Err(DurationError);
        }
        let mut nanos = integer * unit;
        // Preserve Go's operation order: f * (unit / scale), not f / scale * unit.
        nanos += (fraction as f64 * (unit as f64 / scale)) as u64;
        if nanos > LIMIT {
            return Err(DurationError);
        }
        total = total.checked_add(nanos).ok_or(DurationError)?;
        if total > LIMIT {
            return Err(DurationError);
        }
    }
    if negative {
        Ok((total as i64).wrapping_neg())
    } else {
        i64::try_from(total).map_err(|_| DurationError)
    }
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
            assert_eq!(parse_go_duration(text), Err(DurationError), "{text}");
        }
    }
}
