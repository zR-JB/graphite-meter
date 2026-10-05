//! The command line: the client's 22 flags, their validation and the `Config` they give.

use crate::model::{Cadence, Dir, Direction, Stage};
use graphite_meter_proto::{
    catalog::{MAX_SELECTED, ServerId},
    discovery::{LatencyTransport, Protocol, STAGE_LIMITS, ThroughputTransport},
    duration,
    flag::{self, Flag, Kind},
    lane::IDLE_BOUND,
    origin::Origin,
    text::quote,
};
use std::{ffi::OsString, num::IntErrorKind, time::Duration};

pub const DEFAULT_URL: &str = "http://127.0.0.1:7246";
/// Forced and automatic lanes per server and direction stay within this many.
pub const MAX_STREAMS: usize = 14;
const MAX_WARMUP: Duration = Duration::from_secs(4);
const FASTEST_CADENCE: Duration = Duration::from_millis(80);
const SLOWEST_CADENCE: Duration = Duration::from_secs(IDLE_BOUND.as_secs() / 2);

/// One run's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub url: Origin,
    /// Catalogue IDs; empty selects the operator's default.
    pub servers: Vec<ServerId>,
    pub paths: PathChoice,
    /// Distinct, in run order.
    pub stages: Vec<Stage>,
    pub warmup: Duration,
    /// Each stage's measured window, by `Stage as usize`.
    pub durations: [Duration; 4],
    pub streams: Streams,
    pub ping: Cadence,
    pub loaded_ping: Cadence,
    pub loaded_latency: bool,
    pub insecure: bool,
    pub report: bool,
}

/// Forced choices among the discovered paths; `None` is automatic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct PathChoice {
    pub throughput_origin: Option<Origin>,
    pub protocol: Option<Protocol>,
    pub throughput_transport: Option<ThroughputTransport>,
    pub latency_origin: Option<Origin>,
    pub latency_transport: Option<LatencyTransport>,
}

/// Lanes per server and direction: HTTP/1.1's automatic count, and a forced count for every path (0: none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Streams {
    pub auto: usize,
    pub forced: usize,
}

/// What a path check depends on; a run reuses a check whose key is equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepKey {
    url: Origin,
    servers: Vec<ServerId>,
    paths: PathChoice,
    cadences: [Cadence; 2],
    streams: Streams,
    insecure: bool,
    latency: bool,
    checkpoints: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            url: Origin::parse(DEFAULT_URL).expect("the default origin parses"),
            servers: Vec::new(),
            paths: PathChoice::default(),
            stages: vec![Stage::Latency, Stage::Download, Stage::Upload],
            warmup: Duration::from_millis(800),
            durations: [4, 10, 10, 10].map(Duration::from_secs),
            streams: Streams { auto: 6, forced: 0 },
            ping: Cadence::ReplyDriven,
            loaded_ping: Cadence::Every(Duration::from_millis(250)),
            loaded_latency: true,
            insecure: false,
            report: false,
        }
    }
}

impl Config {
    pub fn duration(&self, stage: Stage) -> Duration {
        self.durations[stage as usize]
    }

    /// The stages to run with their windows.
    pub fn plan(&self) -> Vec<(Stage, Duration)> {
        self.stages.iter().map(|&stage| (stage, self.duration(stage))).collect()
    }

    pub fn key(&self) -> PrepKey {
        let mut servers = self.servers.clone();
        servers.sort_unstable();
        PrepKey {
            url: self.url.clone(),
            servers,
            paths: self.paths.clone(),
            cadences: [self.ping, self.loaded_ping],
            streams: self.streams,
            insecure: self.insecure,
            latency: self.probes(),
            checkpoints: self.uploads(),
        }
    }

    /// Whether a stage probes latency: the latency stage, or loaded latency during transfers.
    pub fn probes(&self) -> bool {
        let transfers = self.stages.iter().any(|stage| !stage.directions().is_empty());
        self.stages.contains(&Stage::Latency) || self.loaded_latency && transfers
    }

    /// Whether a stage uploads, which takes receiver checkpoints.
    pub fn uploads(&self) -> bool {
        self.stages.iter().any(|stage| stage.moves(Direction::Up))
    }

    /// Lanes per direction on a fetch-stream path of `protocol`, or a WebTransport one.
    pub fn lanes(&self, protocol: Protocol, transport: ThroughputTransport) -> Dir<usize> {
        let (down, up) = match (self.streams.forced, transport, protocol) {
            (0, ThroughputTransport::FetchStream, Protocol::Http2) => (1, 4),
            (0, ThroughputTransport::FetchStream, Protocol::Http3) => (1, 1),
            (0, ThroughputTransport::FetchStream, _) => (self.streams.auto, self.streams.auto),
            (0, ..) => (1, 1),
            (forced, ..) => (forced, forced),
        };
        Dir { down, up }
    }

    /// The checks made before the path choices are read.
    fn validate(&self) -> Result<(), String> {
        if self.stages.is_empty() {
            return Err("select at least one stage: latency, download, upload or bidirectional".into());
        }
        if self.warmup > MAX_WARMUP {
            return Err("warmup must be from 0s to 4s".into());
        }
        let outside = |stage: &Stage| !STAGE_LIMITS.contains(&self.duration(*stage));
        if let Some(stage) = Stage::ALL.iter().find(|stage| outside(stage)) {
            return Err(format!("{} duration must be from 1s to 24h", stage.name()));
        }
        let fast = |cadence| matches!(cadence, Cadence::Every(spacing) if spacing < FASTEST_CADENCE);
        if fast(self.ping) || fast(self.loaded_ping) {
            return Err("latency cadence must be reply-driven or at least 80ms".into());
        }
        if self.streams.forced > MAX_STREAMS {
            return Err(format!(
                "forced streams must be from 1 to {MAX_STREAMS} per server and direction, or 0 for automatic"
            ));
        }
        if !(1..=MAX_STREAMS).contains(&self.streams.auto) {
            return Err(format!("the automatic stream maximum must be from 1 to {MAX_STREAMS} per direction"));
        }
        Ok(())
    }
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Help,
    Version,
    Legal,
    Run(Box<Config>),
}

/// A refused command line: a flag error goes out with the usage, any other without.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Flag(String),
    Argument(String),
}

/// Reads the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, Refusal> {
    let mut flags = Flags::default();
    let arguments = match flag::parse(&FLAGS, &mut flags, args) {
        Ok(flag::Parsed::Help) => return Ok(Parsed::Help),
        Ok(flag::Parsed::Arguments(arguments)) => arguments,
        Err(error) => return Err(Refusal::Flag(error.0)),
    };
    if flags.legal {
        return Ok(Parsed::Legal);
    }
    if flags.version {
        return Ok(Parsed::Version);
    }
    if let Some(argument) = arguments.first() {
        return Err(Refusal::Argument(format!("unexpected argument {}", quote(&argument.to_string_lossy()))));
    }
    let config = flags.finish().map_err(Refusal::Argument)?;
    Ok(Parsed::Run(Box::new(config)))
}

/// The usage text for `program`.
pub fn usage(program: &str) -> String {
    format!("Usage of {program}:\n{}", flag::defaults(&FLAGS, &Flags::default()))
}

/// What the flags set; path choices stay text until every flag is read, so the last one given counts.
struct Flags {
    config: Config,
    protocol: String,
    throughput_transport: String,
    latency_transport: String,
    legal: bool,
    version: bool,
}

impl Default for Flags {
    fn default() -> Self {
        let auto = || "auto".to_owned();
        Self {
            config: Config::default(),
            protocol: auto(),
            throughput_transport: auto(),
            latency_transport: auto(),
            legal: false,
            version: false,
        }
    }
}

impl Flags {
    fn finish(mut self) -> Result<Config, String> {
        self.config.validate()?;
        let paths = &mut self.config.paths;
        let protocols = [Protocol::Http1, Protocol::Http2, Protocol::Http3];
        paths.protocol = choice("throughput protocol", &self.protocol, &protocols, Protocol::name)?;
        let transports = [ThroughputTransport::FetchStream, ThroughputTransport::WebTransport];
        let transport = &self.throughput_transport;
        paths.throughput_transport = choice("throughput transport", transport, &transports, ThroughputTransport::name)?;
        let transports = [LatencyTransport::WebSocket, LatencyTransport::WebTransport];
        let transport = &self.latency_transport;
        paths.latency_transport = choice("latency transport", transport, &transports, LatencyTransport::name)?;
        let spacing = |cadence| match cadence {
            Cadence::Every(spacing) => spacing,
            Cadence::ReplyDriven => Duration::ZERO,
        };
        if spacing(self.config.ping).max(spacing(self.config.loaded_ping)) > SLOWEST_CADENCE {
            return Err("latency interval must be at most 15s, half the server's 30s lane idle bound".into());
        }
        Ok(self.config)
    }
}

/// The choice named `text`, `auto` or empty being none.
fn choice<T: Copy>(what: &str, text: &str, choices: &[T], name: fn(T) -> &'static str) -> Result<Option<T>, String> {
    if matches!(text, "" | "auto") {
        return Ok(None);
    }
    if let Some(&found) = choices.iter().find(|&&choice| name(choice) == text) {
        return Ok(Some(found));
    }
    let names: Vec<_> = choices.iter().map(|&choice| name(choice)).collect();
    let (last, others) = names.split_last().expect("choices are named");
    Err(format!("invalid {what} {}: use auto, {}, or {last}", quote(text), others.join(", ")))
}

fn boolean(text: &str) -> Result<bool, String> {
    flag::parse_bool(text).ok_or_else(|| "parse error".into())
}

/// A decimal count; a negative one reads as a count validation refuses.
fn count(text: &str) -> Result<usize, String> {
    match text.parse::<i64>() {
        Ok(count) => Ok(usize::try_from(count).unwrap_or(usize::MAX)),
        Err(error) if matches!(error.kind(), IntErrorKind::PosOverflow | IntErrorKind::NegOverflow) => {
            Err("value out of range".into())
        }
        Err(_) => Err("parse error".into()),
    }
}

/// A duration such as `1m30s`; a negative one reads as `negative`.
fn span(text: &str, negative: Duration) -> Result<Duration, String> {
    let nanos = duration::parse(text).map_err(|_| "parse error")?;
    Ok(u64::try_from(nanos).map_or(negative, Duration::from_nanos))
}

fn origin(text: &str) -> Result<Option<Origin>, String> {
    match text {
        "" | "auto" => Ok(None),
        text => Origin::parse(text).map(Some).map_err(|error| error.to_string()),
    }
}

/// The catalogue origin, empty being the default.
fn url(text: &str) -> Result<Origin, String> {
    match text {
        "" => Ok(Config::default().url),
        text => Origin::parse(text).map_err(|error| error.to_string()),
    }
}

fn cadence(text: &str) -> Result<Cadence, String> {
    let named = [
        ("reply-driven", Cadence::ReplyDriven),
        ("fast", Cadence::Every(FASTEST_CADENCE)),
        ("medium", Cadence::Every(Duration::from_millis(250))),
        ("slow", Cadence::Every(Duration::from_millis(600))),
    ];
    let text = text.trim();
    if let Some((_, cadence)) = named.iter().find(|(name, _)| name.eq_ignore_ascii_case(text)) {
        return Ok(*cadence);
    }
    span(text, Duration::ZERO)
        .map(Cadence::Every)
        .map_err(|_| "use reply-driven, fast, medium, slow, or a duration such as 400ms".into())
}

fn stages(text: &str) -> Result<Vec<Stage>, String> {
    let mut stages = Vec::new();
    for part in text.split(',').map(|part| part.trim().to_lowercase()) {
        stages.push(match part.as_str() {
            "" => continue,
            "latency" | "ping" => Stage::Latency,
            "download" | "down" => Stage::Download,
            "upload" | "up" => Stage::Upload,
            "bidirectional" | "bidi" => Stage::Bidirectional,
            _ => {
                return Err(format!("unknown stage {}: use latency, download, upload, or bidirectional", quote(&part)));
            }
        });
    }
    stages.sort_unstable();
    stages.dedup();
    Ok(stages)
}

fn server(servers: &mut Vec<ServerId>, text: &str) -> Result<(), String> {
    let refused = || format!("select one to {MAX_SELECTED} different server IDs");
    if text.is_empty() || servers.len() >= MAX_SELECTED || servers.iter().any(|known| known.as_str() == text) {
        return Err(refused());
    }
    let id = ServerId::parse(text).ok_or("a server ID has 1 to 64 letters, digits, dots, underscores or hyphens")?;
    servers.push(id);
    Ok(())
}

fn show_origin(origin: &Option<Origin>) -> String {
    origin.as_ref().map_or_else(|| "auto".into(), Origin::to_string)
}

/// One flag row; `show` gives the value its usage default prints.
macro_rules! row {
    ($name:literal, $kind:ident, $usage:expr, |$flags:ident, $text:ident| $set:expr, |$shown:ident| $show:expr) => {
        Flag {
            name: $name,
            kind: Kind::$kind,
            usage: $usage,
            env: None,
            set: |$flags: &mut Flags, $text: &str| {
                $set;
                Ok(())
            },
            show: |$shown: &Flags| $show,
        }
    };
}

/// A stage's window; a negative one reads as one validation refuses.
macro_rules! stage_duration {
    ($name:literal, $stage:ident, $usage:literal) => {
        row!(
            $name,
            Duration,
            $usage,
            |f, v| f.config.durations[Stage::$stage as usize] = span(v, Duration::ZERO)?,
            |f| duration::format(f.config.durations[Stage::$stage as usize])
        )
    };
}

#[rustfmt::skip]
const FLAGS: [Flag<Flags>; 22] = [
    row!("auto-streams", Int, "maximum H1 streams per direction",
        |f, v| f.config.streams.auto = count(v)?, |f| f.config.streams.auto.to_string()),
    stage_duration!("bidirectional-duration", Bidirectional, "bidirectional measurement duration"),
    stage_duration!("download-duration", Download, "download measurement duration"),
    row!("insecure", Bool, "skip TLS certificate verification",
        |f, v| f.config.insecure = boolean(v)?, |f| f.config.insecure.to_string()),
    stage_duration!("latency-duration", Latency, "latency measurement duration"),
    row!("latency-origin", String, "latency origin from discovery, or auto",
        |f, v| f.config.paths.latency_origin = origin(v)?, |f| show_origin(&f.config.paths.latency_origin)),
    row!("latency-transport", String, "latency transport: auto, websocket, or webtransport",
        |f, v| v.clone_into(&mut f.latency_transport), |f| f.latency_transport.clone()),
    row!("legal", Bool, "print the licences of the bundled software and exit",
        |f, v| f.legal = boolean(v)?, |f| f.legal.to_string()),
    row!("loaded-latency", Bool, "measure latency while transfer stages are loaded",
        |f, v| f.config.loaded_latency = boolean(v)?, |f| f.config.loaded_latency.to_string()),
    row!("loaded-ping", Value, "loaded latency cadence (default medium): reply-driven, fast, medium, slow, or a \
        duration from 80ms to 15s", |f, v| f.config.loaded_ping = cadence(v)?, |_f| String::new()),
    row!("ping", Value, "idle latency cadence (default reply-driven): reply-driven, fast, medium, slow, or a \
        duration from 80ms to 15s", |f, v| f.config.ping = cadence(v)?, |_f| String::new()),
    row!("report", Bool,
        "run once without the interface and print the final report (automatic when stdout is not a terminal)",
        |f, v| f.config.report = boolean(v)?, |f| f.config.report.to_string()),
    row!("server", Value, "selected catalogue ID (repeat up to 4 times; omission uses operator defaults)",
        |f, v| server(&mut f.config.servers, v)?, |_f| String::new()),
    row!("stages", Value, "comma-separated stages: latency (ping), download (down), upload (up), bidirectional (bidi) \
        (default latency,download,upload)", |f, v| f.config.stages = stages(v)?, |_f| String::new()),
    row!("streams", Int, "force exact streams per server and direction (0 = automatic; at most 14)",
        |f, v| f.config.streams.forced = count(v)?, |f| f.config.streams.forced.to_string()),
    row!("throughput-origin", String, "throughput origin from discovery, or auto",
        |f, v| f.config.paths.throughput_origin = origin(v)?, |f| show_origin(&f.config.paths.throughput_origin)),
    row!("throughput-protocol", String, "protocol for a negotiated throughput origin: auto, http1, http2, or http3",
        |f, v| v.clone_into(&mut f.protocol), |f| f.protocol.clone()),
    row!("throughput-transport", String, "throughput transport: auto, fetch-stream, or webtransport",
        |f, v| v.clone_into(&mut f.throughput_transport), |f| f.throughput_transport.clone()),
    stage_duration!("upload-duration", Upload, "upload measurement duration"),
    row!("url", String, "origin of the operator server catalogue",
        |f, v| f.config.url = url(v)?, |f| f.config.url.to_string()),
    row!("version", Bool, "print version and exit", |f, v| f.version = boolean(v)?, |f| f.version.to_string()),
    row!("warmup", Duration, "per-stage warmup duration",
        |f, v| f.config.warmup = span(v, Duration::MAX)?, |f| duration::format(f.config.warmup)),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &str) -> Result<Config, String> {
        run_args(&args.split_whitespace().collect::<Vec<_>>())
    }

    fn run_args(args: &[&str]) -> Result<Config, String> {
        match parse(args.iter().map(OsString::from)) {
            Ok(Parsed::Run(config)) => Ok(*config),
            Ok(other) => panic!("{args:?}: {other:?}"),
            Err(Refusal::Flag(error) | Refusal::Argument(error)) => Err(error),
        }
    }

    #[test]
    fn no_flags_test_the_local_server_with_go_defaults() {
        let config = run("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.url.to_string(), DEFAULT_URL);
        assert_eq!(run("-url=").unwrap().url, config.url);
        let plan = [(Stage::Latency, 4), (Stage::Download, 10), (Stage::Upload, 10)];
        assert_eq!(config.plan(), plan.map(|(stage, seconds)| (stage, Duration::from_secs(seconds))));
        assert!(run("-url http://bücher.example").is_err());
    }

    #[test]
    fn stages_take_names_and_aliases_in_run_order() {
        let all = Stage::ALL.to_vec();
        assert_eq!(run("-stages BIDI,up,down,ping").unwrap().stages, all);
        assert_eq!(
            run_args(&["--stages", " latency ,,download,latency"]).unwrap().stages,
            [Stage::Latency, Stage::Download]
        );
        let unknown = "invalid value \"upload,sideways\" for flag -stages: unknown stage \"sideways\": use latency, \
                       download, upload, or bidirectional";
        assert_eq!(run("-stages upload,sideways").unwrap_err(), unknown);
        let none = "select at least one stage: latency, download, upload or bidirectional";
        assert_eq!(run("-stages ,").unwrap_err(), none);
    }

    #[test]
    fn cadences_take_names_or_spacings_from_80ms_to_15s() {
        let every = |ms| Cadence::Every(Duration::from_millis(ms));
        let cases = [("reply-driven", Cadence::ReplyDriven), ("FAST", every(80)), (" Medium ", every(250))];
        for (text, cadence) in
            cases
                .into_iter()
                .chain([("slow", every(600)), ("80ms", every(80)), ("15s", every(15_000))])
        {
            assert_eq!(run_args(&["-ping", text]).unwrap().ping, cadence, "{text}");
            assert_eq!(run_args(&["-loaded-ping", text]).unwrap().loaded_ping, cadence, "{text}");
        }
        let floor = "latency cadence must be reply-driven or at least 80ms";
        assert_eq!(run("-ping 79ms").unwrap_err(), floor);
        assert_eq!(run("-loaded-ping 0s").unwrap_err(), floor);
        let ceiling = "latency interval must be at most 15s, half the server's 30s lane idle bound";
        assert_eq!(run("-loaded-ping 15001ms").unwrap_err(), ceiling);
        let unknown = "invalid value \"often\" for flag -ping: use reply-driven, fast, medium, slow, or a duration such \
                       as 400ms";
        assert_eq!(run("-ping often").unwrap_err(), unknown);
    }

    #[test]
    fn durations_hold_their_ranges() {
        for stage in Stage::ALL {
            let name = stage.name();
            assert_eq!(run(&format!("-{name}-duration 1s")).unwrap().duration(stage), Duration::from_secs(1));
            assert!(run(&format!("-{name}-duration 24h")).is_ok());
            let refused = format!("{name} duration must be from 1s to 24h");
            for text in ["999ms", "24h0m0.001s", "-1s"] {
                assert_eq!(run(&format!("-{name}-duration {text}")).unwrap_err(), refused, "{text}");
            }
        }
        assert_eq!(run("-warmup 0").unwrap().warmup, Duration::ZERO);
        assert_eq!(run("-warmup 4s").unwrap().warmup, Duration::from_secs(4));
        for text in ["4001ms", "-1ns"] {
            assert_eq!(run(&format!("-warmup={text}")).unwrap_err(), "warmup must be from 0s to 4s");
        }
        assert_eq!(run("-warmup 1").unwrap_err(), "invalid value \"1\" for flag -warmup: parse error");
    }

    #[test]
    fn stream_counts_stay_within_14() {
        assert_eq!(run("-streams 14 -auto-streams 1").unwrap().streams, Streams { auto: 1, forced: 14 });
        let forced = "forced streams must be from 1 to 14 per server and direction, or 0 for automatic";
        for text in ["15", "-1"] {
            assert_eq!(run(&format!("-streams={text}")).unwrap_err(), forced);
        }
        let automatic = "the automatic stream maximum must be from 1 to 14 per direction";
        for text in ["0", "15"] {
            assert_eq!(run(&format!("-auto-streams {text}")).unwrap_err(), automatic);
        }
        let range = "invalid value \"9223372036854775808\" for flag -streams: value out of range";
        assert_eq!(run("-streams 9223372036854775808").unwrap_err(), range);
    }

    #[test]
    fn lanes_follow_the_path_unless_forced() {
        let config = run("-auto-streams 3").unwrap();
        let (fetch, webtransport) = (ThroughputTransport::FetchStream, ThroughputTransport::WebTransport);
        let cases = [
            (Protocol::Http1, fetch, Dir { down: 3, up: 3 }),
            (Protocol::Http2, fetch, Dir { down: 1, up: 4 }),
            (Protocol::Http3, fetch, Dir { down: 1, up: 1 }),
            (Protocol::Http3, webtransport, Dir { down: 1, up: 1 }),
        ];
        for (protocol, transport, lanes) in cases {
            assert_eq!(config.lanes(protocol, transport), lanes, "{protocol:?} {transport:?}");
        }
        let forced = run("-streams 5").unwrap();
        assert_eq!(forced.lanes(Protocol::Http2, webtransport), Dir { down: 5, up: 5 });
    }

    #[test]
    fn servers_repeat_up_to_four_distinct_ids() {
        let config = run("-server a -server b.c -server d_e -server f-g").unwrap();
        let ids: Vec<_> = config.servers.iter().map(ServerId::as_str).collect();
        assert_eq!(ids, ["a", "b.c", "d_e", "f-g"]);
        let refused = "invalid value \"e\" for flag -server: select one to 4 different server IDs";
        assert_eq!(run("-server a -server b -server c -server d -server e").unwrap_err(), refused);
        assert!(
            run("-server a/b")
                .unwrap_err()
                .starts_with("invalid value \"a/b\" for flag -server: ")
        );
    }

    #[test]
    fn the_last_path_choice_given_counts() {
        let config = run("-throughput-protocol h9 -throughput-protocol http2 -latency-transport auto").unwrap();
        assert_eq!(config.paths.protocol, Some(Protocol::Http2));
        assert_eq!(config.paths.latency_transport, None);
        let config = run("-throughput-transport webtransport -latency-transport webtransport").unwrap();
        assert_eq!(config.paths.throughput_transport, Some(ThroughputTransport::WebTransport));
        assert_eq!(config.paths.latency_transport, Some(LatencyTransport::WebTransport));
        let refused = "invalid latency transport \"datagram\": use auto, websocket, or webtransport";
        assert_eq!(run("-latency-transport websocket -latency-transport datagram").unwrap_err(), refused);
        let origin = run("-throughput-origin https://a.example -throughput-origin auto").unwrap();
        assert_eq!(origin.paths.throughput_origin, None);
        let origin = run("-latency-origin HTTPS://A.example:443")
            .unwrap()
            .paths
            .latency_origin;
        assert_eq!(origin.unwrap().to_string(), "https://a.example");
    }

    #[test]
    fn the_preparation_key_ignores_durations_and_server_order() {
        let key = |args| run(args).unwrap().key();
        let base = key("-server a -server b");
        let timed = "-server b -server a -warmup 2s -latency-duration 9s -download-duration 1s -upload-duration 1m \
                     -bidirectional-duration 3s";
        assert_eq!(key(timed), base);
        assert_eq!(key("-server a -server b -stages ping,down,up -loaded-latency=false"), base);
        assert_ne!(key("-server a"), base);
        assert_ne!(
            key("-server a -server b -stages download,upload"),
            key("-server a -server b -stages down")
        );
        assert_ne!(
            key("-server a -server b -stages down -loaded-latency=false"),
            key("-server a -server b -stages down")
        );
        assert_ne!(key("-server a -server b -insecure"), base);
    }
}
