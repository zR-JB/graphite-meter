//! Native client flags, read as Go's `flag` package reads them. Parsing has no network or terminal side effects.
use crate::{Error, config::Config, model::Stage, vocabulary::CADENCES};
use graphite_meter_core::{
    catalog::MAX_SELECTED_SERVERS,
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    duration::parse_go_duration,
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

/// What the flags set: the run, and the flags that act once every flag has parsed.
#[derive(Default)]
struct Parsed {
    config: Config,
    report: bool,
    version: bool,
    legal: bool,
    /// Go checks the path choices after parsing, so the last one given counts.
    paths: [String; 3],
}

/// A boolean flag, which takes a value only inline, or a flag with a value, whose error is the
/// reason Go's `invalid value` message gives.
enum Flag {
    Toggle(fn(&mut Parsed) -> &mut bool),
    Value(fn(&mut Parsed, &str) -> Result<(), String>),
}

/// Go's flag set: what each flag sets in the parse `p` from its value `v`, or None for a flag it
/// does not define.
fn defined(name: &str) -> Option<Flag> {
    use Flag::{Toggle, Value};
    Some(match name {
        "report" => Toggle(|p| &mut p.report),
        "insecure" => Toggle(|p| &mut p.config.insecure),
        "loaded-latency" => Toggle(|p| &mut p.config.loaded_latency),
        "version" => Toggle(|p| &mut p.version),
        // Go defines -legal but prints notices only for an exact --legal; every spelling prints them here.
        "legal" => Toggle(|p| &mut p.legal),
        // Go reads an empty origin as its default.
        "url" => Value(|p, v| match v {
            "" => put(&mut p.config.url, Config::default().url),
            _ => put(&mut p.config.url, v.into()),
        }),
        "server" => Value(|p, id| {
            let servers = &mut p.config.servers;
            if id.is_empty() || servers.len() >= MAX_SELECTED_SERVERS || servers.iter().any(|existing| existing == id) {
                return Err(format!("select one to {MAX_SELECTED_SERVERS} different server IDs"));
            }
            servers.push(id.into());
            Ok(())
        }),
        "throughput-origin" => Value(|p, v| put(&mut p.config.throughput_origin, automatic(v))),
        "throughput-protocol" => Value(|p, v| put(&mut p.paths[0], v.into())),
        "throughput-transport" => Value(|p, v| put(&mut p.paths[1], v.into())),
        "latency-origin" => Value(|p, v| put(&mut p.config.latency_origin, automatic(v))),
        "latency-transport" => Value(|p, v| put(&mut p.paths[2], v.into())),
        "stages" => Value(|p, v| put(&mut p.config.stages, stages(v)?)),
        // Go refuses a negative duration in validation; the nearest invalid value keeps that message.
        "warmup" => Value(|p, v| put(&mut p.config.warmup, duration(v, Duration::MAX)?)),
        "latency-duration" => Value(|p, v| put(&mut p.config.latency_duration, duration(v, Duration::ZERO)?)),
        "download-duration" => Value(|p, v| put(&mut p.config.download_duration, duration(v, Duration::ZERO)?)),
        "upload-duration" => Value(|p, v| put(&mut p.config.upload_duration, duration(v, Duration::ZERO)?)),
        "bidirectional-duration" => {
            Value(|p, v| put(&mut p.config.bidirectional_duration, duration(v, Duration::ZERO)?))
        }
        "auto-streams" => Value(|p, v| put(&mut p.config.auto_streams, count(v)?)),
        "streams" => Value(|p, v| put(&mut p.config.streams, count(v)?)),
        "ping" => Value(|p, v| put(&mut p.config.ping_interval, cadence(v)?)),
        "loaded-ping" => Value(|p, v| put(&mut p.config.loaded_ping_interval, cadence(v)?)),
        _ => return None,
    })
}

fn put<T>(target: &mut T, value: T) -> Result<(), String> {
    *target = value;
    Ok(())
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, Error> {
    let args = args.into_iter().collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--legal") {
        return Ok(Action::Legal);
    }
    let mut args = args.into_iter();
    let mut parsed = Parsed::default();
    let mut argument = None;
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
        match defined(name).ok_or_else(|| FlagError(format!("flag provided but not defined: -{name}")))? {
            Flag::Toggle(toggle) => {
                let value = inline.unwrap_or("true");
                *toggle(&mut parsed) = boolean(value)
                    .ok_or_else(|| FlagError(format!("invalid boolean value {value:?} for -{name}: parse error")))?;
            }
            Flag::Value(apply) => {
                let value = match inline {
                    Some(value) => value.to_owned(),
                    None => args
                        .next()
                        .ok_or_else(|| FlagError(format!("flag needs an argument: -{name}")))?
                        .into_string()
                        .map_err(|_| "flag values must be valid UTF-8")?,
                };
                apply(&mut parsed, &value)
                    .map_err(|reason| FlagError(format!("invalid value {value:?} for flag -{name}: {reason}")))?;
            }
        }
    }
    // Like Go's -version, these act once every flag has parsed, before arguments and settings are checked.
    if parsed.version {
        return Ok(Action::Version);
    }
    if parsed.legal {
        return Ok(Action::Legal);
    }
    if let Some(argument) = argument {
        return Err(format!("unexpected argument {argument:?}").into());
    }
    let mut config = parsed.config;
    config.validate()?;
    let [protocol, throughput, latency] = &parsed.paths;
    paths(&mut config, protocol, throughput, latency)?;
    Ok(Action::Run {
        config: Box::new(config),
        report: parsed.report,
    })
}

/// Go reads an empty path choice as auto and refuses an unknown one with the settings, after every flag has parsed.
fn paths(config: &mut Config, protocol: &str, throughput: &str, latency: &str) -> Result<(), Error> {
    config.throughput_protocol = match protocol {
        "" | "auto" => None,
        "http1" => Some(Protocol::Http1),
        "http2" => Some(Protocol::Http2),
        "http3" => Some(Protocol::Http3),
        _ => return Err(format!("invalid throughput protocol {protocol:?}: use auto, http1, http2, or http3").into()),
    };
    config.throughput_transport = match throughput {
        "" | "auto" => None,
        "fetch-stream" => Some(ThroughputTransport::FetchStream),
        "webtransport" => Some(ThroughputTransport::WebTransport),
        _ => {
            return Err(format!(
                "invalid throughput transport {throughput:?}: use auto, fetch-stream, or webtransport"
            )
            .into());
        }
    };
    config.latency_transport = match latency {
        "" | "auto" => None,
        "websocket" => Some(LatencyTransport::WebSocket),
        "webtransport" => Some(LatencyTransport::WebTransport),
        _ => return Err(format!("invalid latency transport {latency:?}: use auto, websocket, or webtransport").into()),
    };
    Ok(())
}

fn automatic(value: &str) -> Option<String> {
    (!matches!(value, "" | "auto")).then(|| value.into())
}

fn boolean(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

fn cadence(value: &str) -> Result<Duration, &'static str> {
    let name = value.trim();
    match CADENCES.iter().find(|(key, ..)| key.eq_ignore_ascii_case(name)) {
        Some((.., interval)) => Ok(*interval),
        // Zero means reply-driven here but is a fixed cadence in Go, so a zero or negative duration reads as
        // 1 ns, which validation refuses as Go does.
        None => Ok(duration(name, Duration::ZERO)
            .map_err(|_| "use reply-driven, fast, medium, slow, or a duration such as 400ms")?
            .max(Duration::from_nanos(1))),
    }
}

/// Go's `flag.IntVar` reading a decimal count; a negative one reads as `usize::MAX` for validation to refuse.
fn count(value: &str) -> Result<usize, &'static str> {
    match value.parse::<i64>() {
        Ok(count) => Ok(usize::try_from(count).unwrap_or(usize::MAX)),
        Err(error) if matches!(error.kind(), IntErrorKind::PosOverflow | IntErrorKind::NegOverflow) => {
            Err("value out of range")
        }
        Err(_) => Err("parse error"),
    }
}

/// Go's `flag.DurationVar`; a negative duration reads as `negative`.
fn duration(value: &str, negative: Duration) -> Result<Duration, &'static str> {
    let nanos = parse_go_duration(value).map_err(|_| "parse error")?;
    Ok(u64::try_from(nanos).map_or(negative, Duration::from_nanos))
}

fn stages(value: &str) -> Result<Vec<Stage>, String> {
    let mut stages = value
        .split(',')
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .map(|part| match part.as_str() {
            "latency" | "ping" => Ok(Stage::Latency),
            "download" | "down" => Ok(Stage::Download),
            "upload" | "up" => Ok(Stage::Upload),
            "bidirectional" | "bidi" => Ok(Stage::Bidirectional),
            _ => Err(format!(
                "unknown stage {part:?}: use latency, download, upload, or bidirectional"
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    stages.sort_unstable();
    stages.dedup();
    Ok(stages)
}

pub const HELP: &str = "Graphite Meter experimental Rust client

  -url ORIGIN                   Operator catalogue (http://127.0.0.1:7246)
  -server ID                    Select server; repeat up to four times
  -throughput-origin ORIGIN     Advertised origin, or auto
  -throughput-protocol PROTOCOL auto, http1, http2, http3
  -throughput-transport TYPE    auto, fetch-stream, webtransport
  -latency-origin ORIGIN        Advertised origin, or auto
  -latency-transport TYPE       auto, websocket, webtransport
  -stages LIST                  latency,download,upload,bidirectional
  -warmup DURATION              Per-stage warmup (800ms)
  -latency-duration DURATION    Latency measurement (4s)
  -download-duration DURATION   Download measurement (10s)
  -upload-duration DURATION     Upload measurement (10s)
  -bidirectional-duration DURATION  Bidirectional measurement (10s)
  -auto-streams COUNT           Automatic HTTP/1 stream limit (6)
  -streams COUNT                Streams per server and direction (0 = automatic)
  -ping CADENCE                 Idle: reply-driven, fast, medium, slow, or duration (reply-driven)
  -loaded-ping CADENCE          Loaded: same choices (medium)
  -loaded-latency=BOOL           Measure latency under load (true)
  -insecure                     Skip TLS certificate verification
  -report                       Run once and print the final report
  -version                      Print version
  --legal                       Print dependency notices

Both single-dash and double-dash flags are accepted.
";

#[cfg(test)]
mod tests {
    use super::*;

    /// A script's unset variable keeps Go's default, as its normalized configuration does.
    #[test]
    fn empty_origins_and_path_choices_read_as_defaults() -> Result<(), Error> {
        let empty = [
            "-url",
            "",
            "-throughput-origin=",
            "-latency-origin=",
            "-throughput-protocol=",
            "-throughput-transport=",
            "-latency-transport=",
        ];
        let Action::Run { config, .. } = parse(empty.map(OsString::from))? else {
            return Err("empty values did not start a run".into());
        };
        assert_eq!(*config, Config::default());
        Ok(())
    }
}
