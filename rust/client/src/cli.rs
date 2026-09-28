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

const VALUE_FLAGS: [&str; 17] = [
    "url",
    "server",
    "throughput-origin",
    "throughput-protocol",
    "throughput-transport",
    "latency-origin",
    "latency-transport",
    "stages",
    "warmup",
    "latency-duration",
    "download-duration",
    "upload-duration",
    "bidirectional-duration",
    "auto-streams",
    "streams",
    "ping",
    "loaded-ping",
];

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, Error> {
    let args = args.into_iter().collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--legal") {
        return Ok(Action::Legal);
    }
    let mut args = args.into_iter();
    let mut config = Config::default();
    let (mut report, mut version, mut legal) = (false, false, false);
    // Go checks the path choices after parsing, so the last one given counts.
    let [mut protocol, mut throughput_transport, mut latency_transport] = ["auto"; 3].map(String::from);
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
        let toggle = match name {
            "report" => Some(&mut report),
            "insecure" => Some(&mut config.insecure),
            "loaded-latency" => Some(&mut config.loaded_latency),
            "version" => Some(&mut version),
            // Go defines -legal but prints notices only for an exact --legal; every spelling prints them here.
            "legal" => Some(&mut legal),
            "help" | "h" => return Ok(Action::Help),
            // The name is checked before a value is read.
            _ if !VALUE_FLAGS.contains(&name) => {
                return Err(FlagError(format!("flag provided but not defined: -{name}")).into());
            }
            _ => None,
        };
        if let Some(toggle) = toggle {
            let value = inline.unwrap_or("true");
            *toggle = boolean(value)
                .ok_or_else(|| FlagError(format!("invalid boolean value {value:?} for -{name}: parse error")))?;
            continue;
        }
        let value = match inline {
            Some(value) => value.to_owned(),
            None => args
                .next()
                .ok_or_else(|| FlagError(format!("flag needs an argument: -{name}")))?
                .into_string()
                .map_err(|_| "flag values must be valid UTF-8")?,
        };
        match name {
            "throughput-protocol" => protocol = value,
            "throughput-transport" => throughput_transport = value,
            "latency-transport" => latency_transport = value,
            _ => set(&mut config, name, &value)
                .map_err(|reason| FlagError(format!("invalid value {value:?} for flag -{name}: {reason}")))?,
        }
    }
    // Like Go's -version, these act once every flag has parsed, before arguments and settings are checked.
    if version {
        return Ok(Action::Version);
    }
    if legal {
        return Ok(Action::Legal);
    }
    if let Some(argument) = argument {
        return Err(format!("unexpected argument {argument:?}").into());
    }
    config.validate()?;
    paths(&mut config, &protocol, &throughput_transport, &latency_transport)?;
    Ok(Action::Run {
        config: Box::new(config),
        report,
    })
}

/// Sets a value flag; an error is the reason Go's `invalid value` message gives.
fn set(config: &mut Config, name: &str, value: &str) -> Result<(), String> {
    match name {
        // Go reads an empty origin as its default.
        "url" if value.is_empty() => config.url = Config::default().url,
        "url" => config.url = value.into(),
        "server" => {
            if value.is_empty()
                || config.servers.len() >= MAX_SELECTED_SERVERS
                || config.servers.iter().any(|id| id == value)
            {
                return Err(format!("select one to {MAX_SELECTED_SERVERS} different server IDs"));
            }
            config.servers.push(value.into());
        }
        "throughput-origin" => config.throughput_origin = automatic(value),
        "latency-origin" => config.latency_origin = automatic(value),
        "stages" => config.stages = stages(value)?,
        // Go refuses a negative duration in validation; the nearest invalid value keeps that message.
        "warmup" => config.warmup = duration(value)?.unwrap_or(Duration::MAX),
        "latency-duration" => config.latency_duration = duration(value)?.unwrap_or_default(),
        "download-duration" => config.download_duration = duration(value)?.unwrap_or_default(),
        "upload-duration" => config.upload_duration = duration(value)?.unwrap_or_default(),
        "bidirectional-duration" => config.bidirectional_duration = duration(value)?.unwrap_or_default(),
        "auto-streams" => config.auto_streams = count(value)?,
        "streams" => config.streams = count(value)?,
        "ping" => config.ping_interval = cadence(value)?,
        "loaded-ping" => config.loaded_ping_interval = cadence(value)?,
        _ => unreachable!("-{name} is not a value flag"),
    }
    Ok(())
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
        None => Ok(duration(name)
            .map_err(|_| "use reply-driven, fast, medium, slow, or a duration such as 400ms")?
            .filter(|interval| !interval.is_zero())
            .unwrap_or(Duration::from_nanos(1))),
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

/// Go's `flag.DurationVar`; a negative duration reads as None.
fn duration(value: &str) -> Result<Option<Duration>, &'static str> {
    let nanos = parse_go_duration(value).map_err(|_| "parse error")?;
    Ok(u64::try_from(nanos).ok().map(Duration::from_nanos))
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
