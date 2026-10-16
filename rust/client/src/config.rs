//! The command line: the client's 22 flags, their validation and the `Config` they give.

use crate::model::{Cadence, Dir, Direction, Stage};
use graphite_meter_proto::{
    catalog::{MAX_SELECTED, ServerId},
    discovery::{LatencyTransport, Protocol, STAGE_LIMITS, ThroughputTransport},
    duration,
    flag::{self, Flag},
    lane::IDLE_BOUND,
    origin::Origin,
    text::quote,
};
use std::{
    ffi::OsString,
    fmt,
    num::{IntErrorKind, NonZero},
    path::PathBuf,
    time::Duration,
};

/// Why settings without a server address cannot run; nothing has a sensible default one.
pub const NO_SERVER: &str = "enter the server's address";
/// Forced and automatic lanes per server and direction stay within this many.
pub const MAX_STREAMS: usize = 14;
const MAX_WARMUP: Duration = Duration::from_secs(4);
const FASTEST_CADENCE: Duration = Duration::from_millis(80);
const SLOWEST_CADENCE: Duration = Duration::from_secs(IDLE_BOUND.as_secs() / 2);

/// One run's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The server whose catalogue lists the test servers; none until one is given.
    pub url: Option<Origin>,
    /// Catalogue IDs; empty selects the operator's default.
    pub servers: Vec<ServerId>,
    pub paths: PathChoice,
    /// Distinct, in run order.
    pub stages: Vec<Stage>,
    pub warmup: Duration,
    /// Each stage's measured window, by `Stage::index`.
    pub durations: [Duration; Stage::ALL.len()],
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

/// Lanes per server and direction: HTTP/1.1's automatic count, and a count forced on every path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Streams {
    pub auto: usize,
    pub forced: Option<NonZero<usize>>,
}

impl Streams {
    /// The count the settings show and edit: the forced one, or the automatic maximum.
    pub fn shown(&self) -> usize {
        self.forced.map_or(self.auto, NonZero::get)
    }

    pub fn show(&mut self, count: NonZero<usize>) {
        match &mut self.forced {
            Some(forced) => *forced = count,
            None => self.auto = count.get(),
        }
    }
}

/// What a path check depends on; a run reuses a check whose key is equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepKey {
    url: Option<Origin>,
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
            url: None,
            servers: Vec::new(),
            paths: PathChoice::default(),
            stages: vec![Stage::Latency, Stage::Download, Stage::Upload],
            warmup: Duration::from_millis(800),
            durations: [4, 10, 10, 10].map(Duration::from_secs),
            streams: Streams { auto: 6, forced: None },
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
        self.durations[stage.index()]
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
            (Some(forced), ..) => (forced.get(), forced.get()),
            (None, ThroughputTransport::FetchStream, Protocol::Http2) => (1, 4),
            (None, ThroughputTransport::FetchStream, Protocol::Http3) => (1, 1),
            (None, ThroughputTransport::FetchStream, _) => (self.streams.auto, self.streams.auto),
            (None, ..) => (1, 1),
        };
        Dir { down, up }
    }

    /// Why the settings cannot run, if they cannot.
    pub fn validate(&self) -> Result<(), Invalid> {
        let outside = |stage: &&Stage| !STAGE_LIMITS.contains(&self.duration(**stage));
        let fast = |cadence| matches!(cadence, Cadence::Every(spacing) if spacing < FASTEST_CADENCE);
        match () {
            _ if self.url.is_none() => Err(Invalid::NoServer),
            _ if self.stages.is_empty() => Err(Invalid::NoStages),
            _ if self.warmup > MAX_WARMUP => Err(Invalid::Warmup),
            _ if let Some(stage) = Stage::ALL.iter().find(outside) => Err(Invalid::StageDuration(*stage)),
            _ if fast(self.ping) || fast(self.loaded_ping) => Err(Invalid::Cadence),
            _ if self.streams.forced.is_some_and(|forced| forced.get() > MAX_STREAMS) => Err(Invalid::ForcedStreams),
            _ if !(1..=MAX_STREAMS).contains(&self.streams.auto) => Err(Invalid::AutoStreams),
            _ => Ok(()),
        }
    }
}

/// Why settings cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    NoServer,
    NoStages,
    Warmup,
    StageDuration(Stage),
    Cadence,
    ForcedStreams,
    AutoStreams,
}

impl fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoServer => formatter.write_str(NO_SERVER),
            Self::NoStages => {
                formatter.write_str("select at least one stage: latency, download, upload or bidirectional")
            }
            Self::Warmup => formatter.write_str("warmup must be from 0s to 4s"),
            Self::StageDuration(stage) => write!(formatter, "{} duration must be from 1s to 24h", stage.name()),
            Self::Cadence => formatter.write_str("latency cadence must be reply-driven or at least 80ms"),
            Self::ForcedStreams => write!(
                formatter,
                "forced streams must be from 1 to {MAX_STREAMS} per server and direction, or 0 for automatic"
            ),
            Self::AutoStreams => {
                write!(formatter, "the automatic stream maximum must be from 1 to {MAX_STREAMS} per direction")
            }
        }
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
        Err(error) => return Err(Refusal::Flag(error)),
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
    format!("Usage: {program} [options]\n\nOptions:\n{}", flag::options(&FLAGS, &Flags::default()))
}

/// What the flags set; path choices stay text until every flag is read, so the last one given counts.
#[derive(Default)]
struct Flags {
    config: Config,
    protocol: String,
    throughput_transport: String,
    latency_transport: String,
    legal: bool,
    version: bool,
}

impl Flags {
    /// The settings; a missing server is for `main` to recall or refuse.
    fn finish(mut self) -> Result<Config, String> {
        match self.config.validate() {
            Ok(()) | Err(Invalid::NoServer) => {}
            Err(invalid) => return Err(invalid.to_string()),
        }
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

fn origin(text: &str) -> Result<Option<Origin>, String> {
    match text {
        "" | "auto" => Ok(None),
        text => Origin::parse(text).map(Some).map_err(|error| error.to_string()),
    }
}

/// The catalogue origin, empty being none.
fn url(text: &str) -> Result<Option<Origin>, String> {
    match text {
        "" => Ok(None),
        text => server_origin(text).map(Some),
    }
}

/// The server an entered or pasted address names: its origin, whatever path, query, fragment or credentials came with
/// it. An address without a scheme gets HTTPS, or HTTP where TLS is all but unheard of: loopback, private and
/// link-local addresses, and names only a local network resolves. A failed HTTPS never falls back to HTTP, which would
/// let anyone on the path force a plaintext test.
pub fn server_origin(raw: &str) -> Result<Origin, String> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://").unwrap_or(("", raw));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let scheme = match scheme {
        "" if local(authority) => "http",
        "" => "https",
        scheme => scheme,
    };
    let origin = Origin::parse(&format!("{scheme}://{authority}"))
        .ok()
        .filter(|_| !authority.is_empty());
    origin.ok_or_else(|| "use the server's address, for example https://meter.example or 192.168.1.20:7246".into())
}

/// Whether `authority` names a host on the local network: a loopback, private or link-local address, or a name without
/// a dot or under a local-only suffix.
fn local(authority: &str) -> bool {
    let host = match authority.strip_prefix('[') {
        Some(literal) => literal.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let host = host.to_ascii_lowercase();
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => match ip.to_ipv4_mapped() {
            Some(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
            None => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
        },
        Err(_) => {
            let suffixes = [".localhost", ".local", ".lan", ".home.arpa", ".internal"];
            !host.contains('.') || suffixes.iter().any(|suffix| host.ends_with(suffix))
        }
    }
}

/// Where the server is kept once the user asks to remember it, so a later start needs no `-url`: the user's
/// configuration directory.
pub fn kept_server() -> Option<PathBuf> {
    // A directory that climbs with `..` is refused, so the file stays where it belongs.
    let var = |name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && !path.components().any(|part| part == std::path::Component::ParentDir))
    };
    let home = || var("HOME");
    let directory = match () {
        _ if cfg!(windows) => var("APPDATA"),
        _ if cfg!(target_os = "macos") => home().map(|home| home.join("Library/Application Support")),
        _ => var("XDG_CONFIG_HOME").or_else(|| home().map(|home| home.join(".config"))),
    };
    Some(directory?.join("graphite-meter").join("server"))
}

/// The server kept at `path`, if one is and it still parses.
pub fn recall(path: &std::path::Path) -> Option<Origin> {
    Origin::parse(std::fs::read_to_string(path).ok()?.trim()).ok()
}

/// Deletes the server kept at `path`; one never kept is already forgotten.
pub fn forget(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// Keeps `origin` at `path` for the next start; it is a convenience, so a failure only costs that.
pub fn remember(path: &std::path::Path, origin: &Origin) {
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let mut directory = std::fs::DirBuilder::new();
    #[cfg(unix)]
    directory.mode(0o700);
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create(true).truncate(true);
    #[cfg(unix)]
    file.mode(0o600);
    let parent = path
        .parent()
        .map_or(Ok(()), |parent| directory.recursive(true).create(parent));
    if parent.is_ok()
        && let Ok(mut file) = file.open(path)
    {
        let _ = std::io::Write::write_all(&mut file, format!("{origin}\n").as_bytes());
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
    let spacing = duration::parse(text).map(Cadence::Every);
    spacing.map_err(|_| "use reply-driven, fast, medium, slow, or a duration such as 400ms".into())
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
    ($name:literal, $value:literal, $usage:expr, |$flags:ident, $text:ident| $set:expr, |$shown:ident| $show:expr) => {
        Flag {
            name: $name,
            value: $value,
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

macro_rules! stage_duration {
    ($name:literal, $stage:ident, $usage:literal) => {
        row!(
            $name,
            "duration",
            $usage,
            |f, v| f.config.durations[Stage::$stage.index()] = duration::parse(v)?,
            |f| duration::format(f.config.durations[Stage::$stage.index()])
        )
    };
}

#[rustfmt::skip]
const FLAGS: [Flag<Flags>; 22] = [
    row!("auto-streams", "count", "Maximum H1 streams per direction",
        |f, v| f.config.streams.auto = count(v)?, |f| f.config.streams.auto.to_string()),
    stage_duration!("bidirectional-duration", Bidirectional, "Bidirectional measurement duration"),
    stage_duration!("download-duration", Download, "Download measurement duration"),
    row!("insecure", "", "Skip TLS certificate verification",
        |f, v| f.config.insecure = flag::parse_bool(v)?, |f| f.config.insecure.to_string()),
    stage_duration!("latency-duration", Latency, "Latency measurement duration"),
    row!("latency-origin", "origin", "Latency origin from discovery, or auto",
        |f, v| f.config.paths.latency_origin = origin(v)?, |f| show_origin(&f.config.paths.latency_origin)),
    row!("latency-transport", "name", "Latency transport: auto, websocket, or webtransport",
        |f, v| v.clone_into(&mut f.latency_transport), |_f| "auto".into()),
    row!("legal", "", "Print the licenses of the bundled software and exit",
        |f, v| f.legal = flag::parse_bool(v)?, |f| f.legal.to_string()),
    row!("loaded-latency", "", "Measure latency while transfer stages are loaded",
        |f, v| f.config.loaded_latency = flag::parse_bool(v)?, |f| f.config.loaded_latency.to_string()),
    row!("loaded-ping", "cadence", "Loaded latency cadence: reply-driven, fast, medium, slow, or a duration from \
        80 ms to 15 s", |f, v| f.config.loaded_ping = cadence(v)?, |_f| "medium".into()),
    row!("ping", "cadence", "Idle latency cadence: reply-driven, fast, medium, slow, or a duration from 80 ms to \
        15 s", |f, v| f.config.ping = cadence(v)?, |_f| "reply-driven".into()),
    row!("report", "",
        "Run once without the interface and print the final report (automatic when stdout is not a terminal)",
        |f, v| f.config.report = flag::parse_bool(v)?, |f| f.config.report.to_string()),
    row!("server", "id", "Selected catalogue ID (repeat up to 4 times; omission uses operator defaults)",
        |f, v| server(&mut f.config.servers, v)?, |_f| String::new()),
    row!("stages", "list", "Comma-separated stages: latency (ping), download (down), upload (up), bidirectional \
        (bidi)", |f, v| f.config.stages = stages(v)?, |_f| "latency,download,upload".into()),
    row!("streams", "count", "Force exact streams per server and direction (0 = automatic; at most 14)",
        |f, v| f.config.streams.forced = NonZero::new(count(v)?),
        |f| f.config.streams.forced.map_or(0, NonZero::get).to_string()),
    row!("throughput-origin", "origin", "Throughput origin from discovery, or auto",
        |f, v| f.config.paths.throughput_origin = origin(v)?, |f| show_origin(&f.config.paths.throughput_origin)),
    row!("throughput-protocol", "name", "Protocol for a negotiated throughput origin: auto, http1, http2, or http3",
        |f, v| v.clone_into(&mut f.protocol), |_f| "auto".into()),
    row!("throughput-transport", "name", "Throughput transport: auto, fetch-stream, or webtransport",
        |f, v| v.clone_into(&mut f.throughput_transport), |_f| "auto".into()),
    stage_duration!("upload-duration", Upload, "Upload measurement duration"),
    row!("url", "origin", "Origin of the operator server catalogue",
        |f, v| f.config.url = url(v)?, |_f| String::new()),
    row!("version", "", "Print version and exit",
        |f, v| f.version = flag::parse_bool(v)?, |f| f.version.to_string()),
    row!("warmup", "duration", "Per-stage warmup duration",
        |f, v| f.config.warmup = duration::parse(v)?, |f| duration::format(f.config.warmup)),
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
    fn no_flags_take_go_defaults_without_a_server() {
        let config = run("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!((config.url.as_ref(), config.validate()), (None, Err(Invalid::NoServer)));
        assert_eq!(run("-url=").unwrap().url, None);
        let plan = [(Stage::Latency, 4), (Stage::Download, 10), (Stage::Upload, 10)];
        assert_eq!(config.plan(), plan.map(|(stage, seconds)| (stage, Duration::from_secs(seconds))));
        assert!(run("-url http://bücher.example").is_err());
        assert_eq!(run("-url meter.example").unwrap().url, Origin::parse("https://meter.example").ok());
    }

    #[test]
    fn pasted_addresses_name_their_server() {
        for (raw, expected) in [
            ("  https://Meter.Example/#/history\n", "https://meter.example"),
            ("meter.example", "https://meter.example"),
            ("meter.example:8443/servers?x=1", "https://meter.example:8443"),
            ("user:secret@meter.example/", "https://meter.example"),
            ("HTTP://meter.example:80", "http://meter.example"),
            ("192.168.1.20:7246", "http://192.168.1.20:7246"),
            ("[fe80::1]:7246", "http://[fe80::1]:7246"),
            ("nas:7246", "http://nas:7246"),
            ("meter.home.arpa", "http://meter.home.arpa"),
            ("localhost:7246", "http://localhost:7246"),
            ("203.0.113.7", "https://203.0.113.7"),
            ("[::ffff:192.168.1.2]:7246", "http://[::ffff:192.168.1.2]:7246"),
        ] {
            assert_eq!(server_origin(raw).map(|origin| origin.to_string()), Ok(expected.into()), "{raw:?}");
        }
        for raw in ["", "ftp://meter.example", "https://", "two words"] {
            assert!(server_origin(raw).is_err(), "{raw:?}");
        }
    }
}
