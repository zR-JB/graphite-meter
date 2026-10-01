//! Native client flags, read as Go's `flag` package reads them. Parsing has no network or terminal side effects.
use crate::{
    Error,
    config::Config,
    model::Stage,
    vocabulary::{CADENCES, wire},
};
use graphite_meter_core::{
    catalog::MAX_SELECTED_SERVERS,
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    duration::parse_go_duration,
    text::terminal_character,
};
use std::{ffi::OsString, num::IntErrorKind, time::Duration};

#[derive(Debug)]
pub enum Action {
    Run { config: Box<Config>, report: bool },
    Help,
    Version,
    Legal,
}

/// A flag Go's `flag` package refuses: it prints the message and the usage to stderr and exits 2.
#[derive(Debug)]
pub struct FlagError(String);

impl std::fmt::Display for FlagError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for FlagError {}

/// What the flags set: the run, and -legal and -version, which act once every flag has parsed.
#[derive(Default)]
struct Parsed {
    config: Config,
    report: bool,
    legal: bool,
    version: bool,
    /// Go checks the path choices after parsing, so the last one given counts.
    paths: [String; 3],
}

/// A boolean flag, which takes a value only inline, or a flag with a value, whose error is the
/// reason Go's `invalid value` message gives.
enum Flag {
    Toggle(fn(&mut Parsed, bool)),
    Value(fn(&mut Parsed, &str) -> Result<(), String>),
}
use Flag::{Toggle, Value};

/// Go's flag set as `flag.PrintDefaults` lists it: each flag's name, the type it names and its
/// usage, and what it sets in the parse `p` from its value `v`.
#[rustfmt::skip]
const FLAGS: [(&str, &str, &str, Flag); 22] = [
    ("auto-streams", "int", "maximum H1 streams per direction (default 6)",
        Value(|p, v| put(&mut p.config.auto_streams, count(v)?))),
    ("bidirectional-duration", "duration", "bidirectional measurement duration (default 10s)",
        Value(|p, v| put(&mut p.config.bidirectional_duration, duration(v, Duration::ZERO)?))),
    ("download-duration", "duration", "download measurement duration (default 10s)",
        Value(|p, v| put(&mut p.config.download_duration, duration(v, Duration::ZERO)?))),
    ("insecure", "", "skip TLS certificate verification", Toggle(|p, on| p.config.insecure = on)),
    ("latency-duration", "duration", "latency measurement duration (default 4s)",
        Value(|p, v| put(&mut p.config.latency_duration, duration(v, Duration::ZERO)?))),
    ("latency-origin", "string", "latency origin from discovery, or auto (default \"auto\")",
        Value(|p, v| put(&mut p.config.latency_origin, automatic(v)))),
    ("latency-transport", "string", "latency transport: auto, websocket, or webtransport (default \"auto\")",
        Value(|p, v| put(&mut p.paths[2], v.into()))),
    ("legal", "", "print the licences of the bundled software and exit", Toggle(|p, on| p.legal = on)),
    ("loaded-latency", "", "measure latency while transfer stages are loaded (default true)",
        Toggle(|p, on| p.config.loaded_latency = on)),
    ("loaded-ping", "value", "loaded latency cadence (default medium): reply-driven, fast, medium, slow, or a duration \
        from 80ms to 15s", Value(|p, v| put(&mut p.config.loaded_ping_interval, cadence(v)?))),
    ("ping", "value", "idle latency cadence (default reply-driven): reply-driven, fast, medium, slow, or a duration \
        from 80ms to 15s", Value(|p, v| put(&mut p.config.ping_interval, cadence(v)?))),
    ("report", "", "run once without the interface and print the final report (automatic when stdout is not a \
        terminal)", Toggle(|p, on| p.report = on)),
    ("server", "value", "selected catalogue ID (repeat up to 4 times; omission uses operator defaults)",
        Value(|p, id| server(&mut p.config.servers, id))),
    ("stages", "value", "comma-separated stages: latency (ping), download (down), upload (up), bidirectional (bidi) \
        (default latency,download,upload)", Value(|p, v| put(&mut p.config.stages, stages(v)?))),
    ("streams", "int", "force exact streams per server and direction (0 = automatic; at most 14)",
        Value(|p, v| put(&mut p.config.streams, count(v)?))),
    ("throughput-origin", "string", "throughput origin from discovery, or auto (default \"auto\")",
        Value(|p, v| put(&mut p.config.throughput_origin, automatic(v)))),
    ("throughput-protocol", "string",
        "protocol for a negotiated throughput origin: auto, http1, http2, or http3 (default \"auto\")",
        Value(|p, v| put(&mut p.paths[0], v.into()))),
    ("throughput-transport", "string", "throughput transport: auto, fetch-stream, or webtransport (default \"auto\")",
        Value(|p, v| put(&mut p.paths[1], v.into()))),
    ("upload-duration", "duration", "upload measurement duration (default 10s)",
        Value(|p, v| put(&mut p.config.upload_duration, duration(v, Duration::ZERO)?))),
    // Go reads an empty origin as its default.
    ("url", "string", "origin of the operator server catalogue (default \"http://127.0.0.1:7246\")",
        Value(|p, v| put(&mut p.config.url, if v.is_empty() { Config::default().url } else { v.into() }))),
    ("version", "", "print version and exit", Toggle(|p, on| p.version = on)),
    // Go refuses a negative duration in validation; the nearest invalid value keeps that message.
    ("warmup", "duration", "per-stage warmup duration (default 800ms)",
        Value(|p, v| put(&mut p.config.warmup, duration(v, Duration::MAX)?))),
];

fn put<T>(target: &mut T, value: T) -> Result<(), String> {
    *target = value;
    Ok(())
}

/// Go's usage for `program`: its header, then `flag.PrintDefaults`.
pub fn usage(program: &str) -> String {
    let mut usage = format!("Usage of {program}:\n");
    for (name, kind, text, _) in &FLAGS {
        let space = if kind.is_empty() { "" } else { " " };
        usage.push_str(&format!("  -{name}{space}{kind}\n    \t{text}\n"));
    }
    usage
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, Error> {
    let (mut args, mut parsed, mut argument) = (args.into_iter(), Parsed::default(), None);
    while let Some(arg) = args.next() {
        let arg = arg.into_string().map_err(|_| "flags must be valid UTF-8")?;
        // Parsing stops at "--" or at the first argument that is not a flag.
        let flag = match arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            Some("") if arg == "--" => {
                argument = args.next().map(|next| next.to_string_lossy().into_owned());
                break;
            }
            Some(flag) if !flag.is_empty() => flag,
            _ => {
                argument = Some(arg);
                break;
            }
        };
        if flag.starts_with(['-', '=']) {
            return Err(FlagError(format!("bad flag syntax: {arg}")).into());
        }
        let (name, inline) = flag.split_once('=').map_or((flag, None), |(k, v)| (k, Some(v)));
        if matches!(name, "help" | "h") {
            return Ok(Action::Help);
        }
        // The name is checked before a value is read.
        let defined = FLAGS.iter().find(|(defined, ..)| *defined == name);
        match &defined
            .ok_or_else(|| FlagError(format!("flag provided but not defined: -{name}")))?
            .3
        {
            Toggle(toggle) => {
                let value = inline.unwrap_or("true");
                let refused = format!("invalid boolean value {} for -{name}: parse error", quote(value));
                toggle(&mut parsed, boolean(value).ok_or(FlagError(refused))?);
            }
            Value(apply) => {
                let value = match inline {
                    Some(value) => value.to_owned(),
                    None => args
                        .next()
                        .ok_or_else(|| FlagError(format!("flag needs an argument: -{name}")))?
                        .into_string()
                        .map_err(|_| "flag values must be valid UTF-8")?,
                };
                let refused = |reason| FlagError(format!("invalid value {} for flag -{name}: {reason}", quote(&value)));
                apply(&mut parsed, &value).map_err(refused)?;
            }
        }
    }
    // Like Go's -legal and -version, they act once every flag has parsed, before arguments and settings are checked.
    if parsed.legal {
        return Ok(Action::Legal);
    }
    if parsed.version {
        return Ok(Action::Version);
    }
    if let Some(argument) = argument {
        return Err(format!("unexpected argument {}", quote(&argument)).into());
    }
    let mut config = parsed.config;
    config.validate_settings()?;
    let [protocol, throughput, latency] = &parsed.paths;
    let protocols = [Protocol::Http1, Protocol::Http2, Protocol::Http3];
    config.throughput_protocol = choice("throughput protocol", protocol, &protocols)?;
    let transports = [ThroughputTransport::FetchStream, ThroughputTransport::WebTransport];
    config.throughput_transport = choice("throughput transport", throughput, &transports)?;
    let transports = [LatencyTransport::WebSocket, LatencyTransport::WebTransport];
    config.latency_transport = choice("latency transport", latency, &transports)?;
    config.validate_ceiling()?;
    Ok(Action::Run {
        config: Box::new(config),
        report: parsed.report,
    })
}

/// Go reads an empty path choice as auto and a choice by its wire name, and refuses an unknown one
/// with the settings, after every flag has parsed.
fn choice<T: Copy + serde::Serialize>(what: &str, value: &str, choices: &[T]) -> Result<Option<T>, Error> {
    if matches!(value, "" | "auto") {
        return Ok(None);
    }
    let names: Vec<_> = choices.iter().map(|choice| wire(Some(*choice))).collect();
    if let Some(at) = names.iter().position(|name| name == value) {
        return Ok(Some(choices[at]));
    }
    let (last, others) = names.split_last().expect("choices to name");
    let others = others.join(", ");
    Err(format!("invalid {what} {}: use auto, {others}, or {last}", quote(value)).into())
}

fn automatic(value: &str) -> Option<String> {
    (!matches!(value, "" | "auto")).then(|| value.into())
}

fn server(servers: &mut Vec<String>, id: &str) -> Result<(), String> {
    if id.is_empty() || servers.len() >= MAX_SELECTED_SERVERS || servers.iter().any(|existing| existing == id) {
        return Err(format!("select one to {MAX_SELECTED_SERVERS} different server IDs"));
    }
    servers.push(id.into());
    Ok(())
}

/// Go's strconv.ParseBool.
pub(crate) fn boolean(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Go's parsePing. Go's reply-driven interval is -1 ns, so that duration is reply-driven here too;
/// zero and other negative durations read as 1 ns, which validation refuses as Go does.
fn cadence(value: &str) -> Result<Duration, &'static str> {
    let name = value.trim();
    if let Some((.., interval)) = CADENCES.iter().find(|(key, ..)| key.eq_ignore_ascii_case(name)) {
        return Ok(*interval);
    }
    match parse_go_duration(name) {
        Ok(-1) => Ok(Duration::ZERO),
        Ok(nanos) => Ok(Duration::from_nanos(u64::try_from(nanos).unwrap_or(0).max(1))),
        Err(_) => Err("use reply-driven, fast, medium, slow, or a duration such as 400ms"),
    }
}

/// Go's `flag.IntVar`: strconv.ParseInt in base 0, where 0b, 0o, 0x or a leading 0 picks the base
/// and underscores may separate digits. A negative count reads as `usize::MAX` for validation to refuse.
fn count(value: &str) -> Result<usize, &'static str> {
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest.to_ascii_lowercase()),
        None => (false, value.strip_prefix('+').unwrap_or(value).to_ascii_lowercase()),
    };
    let (radix, digits) = match unsigned.as_bytes() {
        [b'0', b'b', _, ..] => (2, &unsigned[2..]),
        [b'0', b'o', _, ..] => (8, &unsigned[2..]),
        [b'0', b'x', _, ..] => (16, &unsigned[2..]),
        [b'0', ..] => (8, &unsigned[1..]),
        _ => (10, &unsigned[..]),
    };
    // Unlike from_str_radix, Go takes no second sign and reads a lone 0 as zero.
    if unsigned.is_empty() || digits.starts_with(['+', '-']) {
        return Err("parse error");
    }
    let plain = digits.replace('_', "");
    let magnitude = match plain.is_empty() && radix == 8 {
        true => Ok(0),
        false => u64::from_str_radix(&plain, radix),
    };
    let magnitude = match magnitude {
        Err(error) if *error.kind() == IntErrorKind::PosOverflow => return Err("value out of range"),
        Err(_) => return Err("parse error"),
        Ok(_) if !underscores(&unsigned) => return Err("parse error"),
        Ok(magnitude) => magnitude,
    };
    match (negative, i64::try_from(magnitude)) {
        (false, Ok(count)) => Ok(usize::try_from(count).unwrap_or(usize::MAX)),
        (true, Ok(0)) => Ok(0),
        (true, _) if magnitude <= 1 << 63 => Ok(usize::MAX),
        _ => Err("value out of range"),
    }
}

/// Go's underscoreOK: an underscore only between digits, or after a base prefix.
fn underscores(unsigned: &str) -> bool {
    let prefixed = matches!(unsigned.as_bytes(), [b'0', b'b' | b'o' | b'x', ..]);
    let (mut previous, start) = if prefixed { (b'0', 2) } else { (b'^', 0) };
    for byte in &unsigned.as_bytes()[start..] {
        previous = match byte {
            b'_' if previous != b'0' => return false,
            b'_' => b'_',
            _ if byte.is_ascii_hexdigit() => b'0',
            _ if previous == b'_' => return false,
            _ => b'!',
        };
    }
    previous != b'_'
}

/// Go's `flag.DurationVar`; a negative duration reads as `negative`.
fn duration(value: &str, negative: Duration) -> Result<Duration, &'static str> {
    let nanos = parse_go_duration(value).map_err(|_| "parse error")?;
    Ok(u64::try_from(nanos).map_or(negative, Duration::from_nanos))
}

const STAGES: &str = "latency, download, upload, or bidirectional";

fn stages(value: &str) -> Result<Vec<Stage>, String> {
    let mut stages = Vec::new();
    for part in value.split(',').map(|part| part.trim().to_lowercase()) {
        stages.push(match part.as_str() {
            "" => continue,
            "latency" | "ping" => Stage::Latency,
            "download" | "down" => Stage::Download,
            "upload" | "up" => Stage::Upload,
            "bidirectional" | "bidi" => Stage::Bidirectional,
            _ => return Err(format!("unknown stage {}: use {STAGES}", quote(&part))),
        });
    }
    stages.sort_unstable();
    stages.dedup();
    Ok(stages)
}

/// Go's %q: the text in double quotes, with Go's escapes for what it does not print.
fn quote(text: &str) -> String {
    let escaped = text.chars().map(|character| {
        let code = u32::from(character);
        match character {
            '"' | '\\' => format!("\\{character}"),
            '\x07' => "\\a".into(),
            '\x08' => "\\b".into(),
            '\x0c' => "\\f".into(),
            '\n' => "\\n".into(),
            '\r' => "\\r".into(),
            '\t' => "\\t".into(),
            '\x0b' => "\\v".into(),
            _ if character == ' ' || terminal_character(character) && !character.is_whitespace() => character.into(),
            _ if code < 0x80 => format!("\\x{code:02x}"),
            _ if code <= 0xffff => format!("\\u{code:04x}"),
            _ => format!("\\U{code:08x}"),
        }
    });
    format!("\"{}\"", escaped.collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script's unset variable keeps Go's default, as its normalized configuration does.
    #[test]
    fn empty_origins_and_path_choices_read_as_defaults() -> Result<(), Error> {
        #[rustfmt::skip]
        let empty = ["-url", "", "-throughput-origin=", "-latency-origin=", "-throughput-protocol=",
            "-throughput-transport=", "-latency-transport="];
        let Action::Run { config, .. } = parse(empty.map(OsString::from))? else {
            return Err("empty values did not start a run".into());
        };
        assert_eq!(*config, Config::default());
        // Go's checkPaths refuses a path choice before the cadence ceiling.
        let refused = parse(["-ping", "20s", "-throughput-transport", "bogus"].map(OsString::from)).err();
        assert!(refused.is_some_and(|error| error.to_string().starts_with("invalid throughput transport")));
        Ok(())
    }

    /// Go's strconv.ParseInt in base 0, its -1 ns reply-driven cadence and its %q, as Go reads
    /// and writes these.
    #[test]
    fn counts_cadences_and_quotes_read_as_go() {
        let (range, max) = (Err("value out of range"), Ok(usize::MAX));
        #[rustfmt::skip]
        let counts = [("010", Ok(8)), ("0x8", Ok(8)), ("0B1_000", Ok(8)), ("0o17", Ok(15)), ("0X1F", Ok(31)),
            ("1_0", Ok(10)), ("0_10", Ok(8)), ("0x_1", Ok(1)), ("+3", Ok(3)), ("0", Ok(0)), ("-0", Ok(0)), ("-1", max),
            ("-9223372036854775808", max), ("9223372036854775807", Ok(9_223_372_036_854_775_807)),
            ("9223372036854775808", range), ("-9223372036854775809", range), ("99999999999999999999x", range)];
        for (text, expected) in counts {
            assert_eq!(count(text), expected, "{text}");
        }
        for text in [
            "08", "1__0", "_1", "1_", "0_", "0x", "0b", "0b2", "++1", "-+1", "", "x9", "0xg",
        ] {
            assert_eq!(count(text), Err("parse error"), "{text}");
        }
        let nanos = Duration::from_nanos;
        #[rustfmt::skip]
        let cadences = [("-1ns", nanos(0)), ("0", nanos(1)), ("-2ns", nanos(1)), (" Fast ", nanos(80_000_000))];
        for (text, interval) in cadences {
            assert_eq!(cadence(text), Ok(interval), "{text}");
        }
        let text = "x\x01y\u{202e}\"é\u{a0}\t\x7f\u{85}😀 \u{200b}";
        // "@" stands for the backslash that begins each of Go's escapes.
        let expected = "\"x@x01y@u202e@\"é@u00a0@t@x7f@u0085😀 @u200b\"".replace('@', "\\");
        assert_eq!(quote(text), expected);
    }
}
