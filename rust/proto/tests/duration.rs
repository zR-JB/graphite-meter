use graphite_meter_proto::duration::{format, parse};
use std::time::Duration;

#[test]
fn durations_parse_exactly() {
    for (text, nanos) in [
        ("0", 0),
        ("0s", 0),
        ("1us", 1_000),
        ("800ms", 800_000_000),
        ("1h30m", 5_400_000_000_000),
        ("1.5h", 5_400_000_000_000),
        ("5m0s", 300_000_000_000),
        ("1h2m3.004005006s", 3_723_004_005_006),
        (".5ms", 500_000),
        ("1.s", 1_000_000_000),
        ("0.1s", 100_000_000),
        ("1.0000000001h", 3_600_000_000_360),
        ("0.333333333333333333333333h", 1_199_999_999_999),
        ("18446744073709551615ns", u64::MAX),
    ] {
        assert_eq!(parse(text), Ok(Duration::from_nanos(nanos)), "{text}");
    }
}

#[test]
fn malformed_negative_and_overlong_durations_are_refused() {
    for text in [
        "", "1", "00", "+1s", " 1s", "1s ", "1e3s", "1d", "1\u{b5}s", ".s", "1s-1s", "１s",
    ] {
        assert_eq!(
            parse(text),
            Err("use a duration such as 800ms, 10s or 1m30s"),
            "{text:?}"
        );
    }
    for text in ["-1s", "-0"] {
        assert_eq!(parse(text), Err("a duration cannot be negative"), "{text}");
    }
    for text in [
        "18446744073709551616ns",
        "5124096h",
        "99999999999999999999999999999999999999999h",
    ] {
        assert_eq!(parse(text), Err("duration too long"), "{text}");
    }
}

#[test]
fn durations_format_as_they_parse() {
    for (nanos, text) in [
        (0, "0s"),
        (1, "1ns"),
        (1_500, "1.5us"),
        (1_234_567, "1.234567ms"),
        (1_500_000_000, "1.5s"),
        (60_000_000_000, "1m"),
        (90_000_000_000, "1m30s"),
        (3_600_000_000_000, "1h"),
        (3_723_004_005_006, "1h2m3.004005006s"),
    ] {
        assert_eq!(format(Duration::from_nanos(nanos)), text, "{nanos}");
        assert_eq!(parse(text), Ok(Duration::from_nanos(nanos)), "{text}");
    }
}
