//! Native client flags. Parsing has no network or terminal side effects.
use crate::{Error, config::Config, model::Stage};
use graphite_meter_core::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    duration::parse_go_duration,
};
use std::{ffi::OsString, time::Duration};

#[derive(Debug)]
pub enum Action {
    Run(Box<Config>),
    Help,
    Version,
    Legal,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, Error> {
    let args = args.into_iter().collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--legal") {
        return Ok(Action::Legal);
    }
    let mut args = args.into_iter();
    let mut config = Config::default();
    while let Some(arg) = args.next() {
        let arg = arg.into_string().map_err(|_| "flags must be valid UTF-8")?;
        let flag = arg
            .strip_prefix("--")
            .or_else(|| arg.strip_prefix('-'))
            .ok_or("unexpected positional argument")?;
        let (name, inline) = flag
            .split_once('=')
            .map_or((flag, None), |(k, v)| (k, Some(v)));
        match name {
            "help" | "h" if inline.is_none() => return Ok(Action::Help),
            "version" => {
                if boolean(inline.unwrap_or("true"))? {
                    return Ok(Action::Version);
                }
                continue;
            }
            "insecure" => {
                config.insecure = boolean(inline.unwrap_or("true"))?;
                continue;
            }
            "loaded-latency" => {
                config.loaded_latency = boolean(inline.unwrap_or("true"))?;
                continue;
            }
            _ => {}
        }
        let value = match inline {
            Some(value) => value.to_owned(),
            None => args
                .next()
                .ok_or_else(|| format!("-{name} requires a value"))?
                .into_string()
                .map_err(|_| "flag values must be valid UTF-8")?,
        };
        match name {
            "url" => config.url = value,
            "server" => config.servers.push(value),
            "throughput-origin" => config.throughput_origin = automatic(value),
            "latency-origin" => config.latency_origin = automatic(value),
            "throughput-protocol" => {
                config.throughput_protocol = match value.as_str() {
                    "auto" => None,
                    "http1" => Some(Protocol::Http1),
                    "http2" => Some(Protocol::Http2),
                    "http3" => Some(Protocol::Http3),
                    _ => {
                        return Err(
                            "throughput protocol must be auto, http1, http2, or http3".into()
                        );
                    }
                }
            }
            "throughput-transport" => {
                config.throughput_transport = match value.as_str() {
                    "auto" => None,
                    "fetch-stream" => Some(ThroughputTransport::FetchStream),
                    "webtransport" => Some(ThroughputTransport::WebTransport),
                    "webtransport-datagram" => Some(ThroughputTransport::WebTransportDatagram),
                    _ => return Err("unknown throughput transport".into()),
                }
            }
            "latency-transport" => {
                config.latency_transport = match value.as_str() {
                    "auto" => None,
                    "websocket" => Some(LatencyTransport::WebSocket),
                    "webtransport" => Some(LatencyTransport::WebTransport),
                    _ => return Err("unknown latency transport".into()),
                }
            }
            "stages" => config.stages = stages(&value)?,
            "warmup" => config.warmup = duration(&value)?,
            "latency-duration" => config.latency_duration = duration(&value)?,
            "download-duration" => config.download_duration = duration(&value)?,
            "upload-duration" => config.upload_duration = duration(&value)?,
            "bidirectional-duration" => config.bidirectional_duration = duration(&value)?,
            "auto-streams" => config.auto_streams = value.parse()?,
            "streams" => config.streams = value.parse()?,
            "ping" => {
                config.ping_interval = match value.trim().to_ascii_lowercase().as_str() {
                    "instant" => Duration::from_millis(80),
                    "medium" => Duration::from_millis(250),
                    "slow" => Duration::from_millis(600),
                    _ => duration(&value)?,
                }
            }
            _ => return Err(format!("unknown flag: -{name}").into()),
        }
    }
    config.validate()?;
    Ok(Action::Run(Box::new(config)))
}

fn automatic(value: String) -> Option<String> {
    if value == "auto" { None } else { Some(value) }
}

fn boolean(value: &str) -> Result<bool, Error> {
    match value {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Ok(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Ok(false),
        _ => Err("invalid boolean value".into()),
    }
}

fn duration(value: &str) -> Result<Duration, Error> {
    let nanos = parse_go_duration(value)?;
    Ok(Duration::from_nanos(
        u64::try_from(nanos).map_err(|_| "duration must not be negative")?,
    ))
}

fn stages(value: &str) -> Result<Vec<Stage>, Error> {
    let mut selected = [false; 4];
    for part in value.split(',') {
        let index = match part.trim().to_ascii_lowercase().as_str() {
            "latency" | "ping" => 0,
            "download" | "down" => 1,
            "upload" | "up" => 2,
            "bidirectional" | "bidi" => 3,
            _ => return Err(format!("unknown measurement stage: {part}").into()),
        };
        selected[index] = true;
    }
    Ok([
        Stage::Latency,
        Stage::Download,
        Stage::Upload,
        Stage::Bidirectional,
    ]
    .into_iter()
    .zip(selected)
    .filter_map(|(stage, enabled)| enabled.then_some(stage))
    .collect())
}

pub const HELP: &str = "Graphite Meter experimental Rust client

  -url ORIGIN                   Operator catalogue (http://127.0.0.1:7246)
  -server ID                    Select server; repeat up to four times
  -throughput-origin ORIGIN     Advertised origin, or auto
  -throughput-protocol PROTOCOL auto, http1, http2, http3
  -throughput-transport TYPE    auto, fetch-stream, webtransport, webtransport-datagram
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
  -ping CADENCE                 instant, medium, slow, or duration (medium)
  -loaded-latency=BOOL           Measure latency under load (true)
  -insecure                     Skip TLS certificate verification
  -version                      Print version
  --legal                       Print dependency notices

Both single-dash and double-dash flags are accepted.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_invocation_preserves_stage_order_and_explicit_choices() {
        let Action::Run(config) = parse(
            [
                "-url",
                "https://meter.example",
                "--server=eu",
                "--server=us",
                "-stages",
                "up,ping,up,bidi",
                "--throughput-protocol=http3",
                "--latency-transport=webtransport",
                "--ping=80ms",
                "--warmup=.5s",
                "--loaded-latency=false",
                "--streams=16",
            ]
            .map(OsString::from),
        )
        .unwrap() else {
            panic!("expected run configuration")
        };
        assert_eq!(
            config.stages,
            [Stage::Latency, Stage::Upload, Stage::Bidirectional]
        );
        assert_eq!(config.servers, ["eu", "us"]);
        assert_eq!(config.throughput_protocol, Some(Protocol::Http3));
        assert_eq!(config.warmup, Duration::from_millis(500));
        assert!(!config.loaded_latency);
        assert_eq!(config.streams, 16);
    }

    #[test]
    fn invalid_measurement_settings_are_not_silently_replaced() {
        for args in [
            vec!["--stages=typo"],
            vec!["--ping=typo"],
            vec!["--warmup=-1s"],
            vec!["--download-duration=0"],
            vec!["--server=a", "--server=a"],
            vec!["--latency-transport=webtransport", "--ping=16s"],
            vec!["--throughput-origin=https://user:secret@example.com"],
        ] {
            assert!(parse(args.into_iter().map(OsString::from)).is_err());
        }
    }
}
