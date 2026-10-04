use graphite_meter_proto::duration::{format, parse};
use std::time::Duration;

#[test]
fn durations_parse_as_go_parses_them() {
    for (text, nanos) in [
        ("0", 0),
        ("+0", 0),
        ("-0", 0),
        ("0ns", 0),
        ("1us", 1_000),
        ("1\u{b5}s", 1_000),
        ("1\u{3bc}s", 1_000),
        ("1h30m", 5_400_000_000_000),
        ("1.5h", 5_400_000_000_000),
        ("15m", 900_000_000_000),
        ("5m0s", 300_000_000_000),
        ("-1ns", -1),
        ("1h2m3.004005006s", 3_723_004_005_006),
        ("-.5ms", -500_000),
        ("+1.s", 1_000_000_000),
        ("1.0000000001h", 3_600_000_000_360),
        ("0.333333333333333333333333h", 1_200_000_000_000),
        ("0.999999999999999999999ns", 1),
        ("0.0000000005s0.0000000005s", 0),
        (".000000000277777777777777777h", 1_000),
        (".9223372036854775808123456789h", 3_320_413_933_267),
        ("9223372036854775807ns", i64::MAX),
        ("-9223372036854775808ns", i64::MIN),
        ("2562047h47m16.854775807s", i64::MAX),
        ("-2562047h47m16.854775808s", i64::MIN),
    ] {
        assert_eq!(parse(text), Ok(nanos), "{text}");
    }
}

#[test]
fn refused_durations_carry_go_messages() {
    for (text, message) in [
        ("", r#"time: invalid duration """#),
        ("+", r#"time: invalid duration "+""#),
        ("00", r#"time: missing unit in duration "00""#),
        ("0.0", r#"time: missing unit in duration "0.0""#),
        ("1", r#"time: missing unit in duration "1""#),
        ("1e3s", r#"time: unknown unit "e" in duration "1e3s""#),
        (" 1s", r#"time: invalid duration " 1s""#),
        ("1s ", r#"time: unknown unit "s " in duration "1s ""#),
        (".s", r#"time: invalid duration ".s""#),
        ("1..0s", r#"time: missing unit in duration "1..0s""#),
        ("1s-1s", r#"time: unknown unit "s-" in duration "1s-1s""#),
        ("1d", r#"time: unknown unit "d" in duration "1d""#),
        ("1S", r#"time: unknown unit "S" in duration "1S""#),
        ("１s", r#"time: invalid duration "\xef\xbc\x91s""#),
        ("1\u{3bc}", r#"time: unknown unit "\xce\xbc" in duration "1\xce\xbc""#),
        ("1\0s", r#"time: unknown unit "\x00s" in duration "1\x00s""#),
        ("1\"s", r#"time: unknown unit "\"s" in duration "1\"s""#),
        ("9223372036854775808ns", r#"time: invalid duration "9223372036854775808ns""#),
        ("-9223372036854775809ns", r#"time: invalid duration "-9223372036854775809ns""#),
        ("9223372036854775807ns1ns", r#"time: invalid duration "9223372036854775807ns1ns""#),
        ("2562047h47m16.854775808s", r#"time: invalid duration "2562047h47m16.854775808s""#),
        ("9223372036854775808h", r#"time: invalid duration "9223372036854775808h""#),
    ] {
        assert_eq!(parse(text).map_err(|error| error.to_string()), Err(message.into()), "{text:?}");
    }
    assert!(parse("9223372036854775808ns9223372036854775808ns").is_err(), "Go wraps this sum to zero");
}

#[test]
fn durations_format_as_go_prints_them() {
    for (nanos, text) in [
        (0, "0s"),
        (1, "1ns"),
        (999, "999ns"),
        (1_000, "1\u{b5}s"),
        (1_500, "1.5\u{b5}s"),
        (999_999, "999.999\u{b5}s"),
        (1_000_000, "1ms"),
        (1_234_567, "1.234567ms"),
        (800_000_000, "800ms"),
        (999_999_999, "999.999999ms"),
        (1_000_000_000, "1s"),
        (1_500_000_000, "1.5s"),
        (4_000_000_000, "4s"),
        (60_000_000_000, "1m0s"),
        (61_000_000_000, "1m1s"),
        (300_000_000_000, "5m0s"),
        (3_599_999_999_999, "59m59.999999999s"),
        (3_600_000_000_000, "1h0m0s"),
        (3_723_004_005_006, "1h2m3.004005006s"),
        (86_400_000_000_000, "24h0m0s"),
        (i64::MAX as u64, "2562047h47m16.854775807s"),
    ] {
        assert_eq!(format(Duration::from_nanos(nanos)), text, "{nanos}");
        assert_eq!(parse(text), Ok(nanos as i64), "{text}");
    }
}
