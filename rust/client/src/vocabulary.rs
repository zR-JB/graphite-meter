//! Go's TUI vocabulary (vocabulary.go) and the formats of its settings (format.go).
use crate::{config::Config, model::Stage};
use graphite_meter_core::{
    discovery::{LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport},
    origin::target_origin,
};
use std::time::Duration;

pub const MISSING: &str = "—";
pub const ADDED_NOTE: &str = "Added: loaded median minus idle median, same server.";
pub const NOT_STARTED: &str = "Not started";
pub const BLOCKED: &str = "Test cannot start";
pub const START_FAILED: &str = "Test could not start";
pub const CHECKING_SIGN_IN: &str = "Checking sign-in";

/// Go's cadences: the flag name, the label and the interval; zero is reply-driven.
pub const CADENCES: [(&str, &str, Duration); 4] = [
    ("reply-driven", "Reply-driven", Duration::ZERO),
    ("fast", "Fast (80 ms)", Duration::from_millis(80)),
    ("medium", "Medium (250 ms)", Duration::from_millis(250)),
    ("slow", "Slow (600 ms)", Duration::from_millis(600)),
];

/// Go's cadenceLabel.
pub fn cadence(interval: Duration) -> String {
    match CADENCES.iter().find(|(.., preset)| *preset == interval) {
        Some((_, label, _)) => (*label).into(),
        None => format!("Custom ({})", setting(interval)),
    }
}

/// Go's fmtSetting: milliseconds below a second, else seconds as short as they print.
pub fn setting(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else {
        format!("{} s", duration.as_secs_f64())
    }
}

/// Go's fmtClock.
pub fn clock(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

/// Go's compactStage.
pub fn compact_stage(stage: Stage) -> &'static str {
    match stage {
        Stage::Bidirectional => "Bi-dir",
        stage => stage.name(),
    }
}

/// Go's populationLabel.
pub fn population_label(stage: Stage) -> String {
    match stage {
        Stage::Latency => "Idle latency".into(),
        stage => format!("Loaded latency · {}", stage.name()),
    }
}

/// Go's compactPopulation.
pub fn compact_population(stage: Stage) -> &'static str {
    match stage {
        Stage::Latency => "Idle",
        Stage::Download => "Loaded down",
        Stage::Upload => "Loaded up",
        Stage::Bidirectional => "Loaded bi-dir",
    }
}

/// A transport's wire name, which Go's settings and labels use; none is "auto".
pub fn wire(transport: Option<impl serde::Serialize>) -> String {
    let name = transport.and_then(|transport| serde_json::to_value(transport).ok());
    name.and_then(|name| name.as_str().map(str::to_owned))
        .unwrap_or_else(|| "auto".into())
}

/// Go's transportLabel of a wire name: WebTransport probes travel as datagrams.
pub fn transport(kind: &str, latency: bool) -> String {
    let label = match kind {
        "fetch-stream" => "Fetch streams",
        "websocket" => "WebSocket",
        "webtransport" if !latency => "WebTransport streams",
        "webtransport" | "webtransport-datagram" => "WebTransport datagrams",
        kind => kind,
    };
    label.into()
}

/// Go's protocolLabel; none is automatic.
pub fn protocol(protocol: Option<Protocol>) -> &'static str {
    match protocol {
        None => "Automatic",
        Some(Protocol::Http1) => "HTTP/1.1",
        Some(Protocol::Http2) => "HTTP/2",
        Some(Protocol::Http3) => "HTTP/3",
        Some(Protocol::Negotiated) => "Negotiated",
    }
}

fn security(origin: &str) -> &'static str {
    match target_origin(origin) {
        Ok(Some(parsed)) if parsed.scheme == "https" => "TLS",
        Ok(Some(_)) => "clear",
        _ => "unknown",
    }
}

/// Go's connectionSummary of a throughput path.
pub fn throughput_path(target: &ThroughputTarget) -> String {
    let (kind, version) = (
        transport(&wire(Some(target.transport)), false),
        protocol(Some(target.protocol)),
    );
    format!("{kind} · {version} · {}", security(&target.base_url))
}

/// Go's connectionSummary of a latency path: WebSocket runs over HTTP/1.1, WebTransport over HTTP/3.
pub fn latency_path(target: &LatencyTarget) -> String {
    let version = match target.transport {
        LatencyTransport::WebSocket => Protocol::Http1,
        LatencyTransport::WebTransport => Protocol::Http3,
    };
    let kind = transport(&wire(Some(target.transport)), true);
    format!("{kind} · {} · {}", protocol(Some(version)), security(&target.base_url))
}

/// Go's streamsLabel for the lanes `Config::lanes` opens on the path.
pub fn streams(config: &Config, target: Option<&ThroughputTarget>) -> String {
    if config.streams > 0 {
        return format!("Forced · {} per direction", config.streams);
    }
    match target.map(|target| (target.transport, target.protocol, config.lanes(target))) {
        Some((ThroughputTransport::WebTransport, ..)) => "Automatic · 1 continuous stream per direction".into(),
        Some((_, Protocol::Http2 | Protocol::Http3, (down, up))) => {
            format!("Automatic · {down} download / {up} upload")
        }
        Some((_, Protocol::Http1, (down, _))) => format!("Automatic · up to {down} per direction"),
        _ => "Automatic".into(),
    }
}
