//! The words and numbers views print: stage, population and path labels, durations, counts and a run's cells.
use crate::{
    measure::{
        format,
        latency::{Population, added},
    },
    model::{Cadence, Direction, Outcome, Stage, Throughput},
    net::{LatencyPath, ThroughputPath},
};
use graphite_meter_proto::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    origin::{Origin, Scheme},
};
use std::time::Duration;

/// What a cell shows without a value.
pub const MISSING: &str = "—";

/// Median, Added, P95, jitter and probe timeouts.
pub fn latency_cells(population: &Population, idle: Option<Duration>) -> Vec<String> {
    let (summary, median) = (population.summary, population.median());
    let mut cells = vec![MISSING.to_owned(); 5];
    if let Some(median) = median {
        cells[0] = ms(median);
        cells[1] = added(Some(median), idle).map_or(MISSING.into(), |added| format!("{} ms", format::added(added)));
    }
    if summary.replies > 0 {
        cells[2] = summary.p95.map_or(MISSING.into(), ms);
    }
    if summary.jitter_pairs > 0 {
        cells[3] = summary.jitter.map_or(MISSING.into(), ms);
    }
    if let Some(ratio) = summary.timeout_ratio() {
        cells[4] = format!("{} / {}", count(summary.timeouts), count(summary.replies + summary.timeouts));
        if ratio >= 0.01 {
            cells[4].push_str(&format!(" ({:.1}%)", ratio * 100.0));
        } else if ratio > 0.0 {
            cells[4].push_str(&format!(" ({:.2}%)", ratio * 100.0));
        }
    }
    cells
}

/// The peak without the mean's unit, bytes, the window and, for uploads, that the receiver timed it.
pub fn throughput_facts(throughput: Throughput, measured: Duration, direction: Direction) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(rate) = throughput.rate.filter(|rate| rate.peak > 0.0) {
        let (peak, mean) = (format::rate(rate.peak), format::rate(rate.mean));
        let unit = mean
            .rsplit_once(' ')
            .map(|(_, unit)| format!(" {unit}"))
            .unwrap_or_default();
        facts.push(format!("peak {}", peak.strip_suffix(&unit).unwrap_or(&peak)));
    }
    facts.push(format::bytes(throughput.bytes));
    facts.extend((!measured.is_zero()).then(|| clock(measured)));
    facts.extend((direction == Direction::Up).then(|| "receiver-timed".to_owned()));
    facts
}

pub fn label(stage: Stage) -> &'static str {
    ["Latency", "Download", "Upload", "Bidirectional"][stage as usize]
}

pub fn compact_stage(stage: Stage) -> &'static str {
    if stage == Stage::Bidirectional { "Bi-dir" } else { label(stage) }
}

pub fn compact_population(stage: Stage) -> &'static str {
    ["Idle", "Loaded down", "Loaded up", "Loaded bi-dir"][stage as usize]
}

pub fn population_label(stage: Stage) -> String {
    match stage {
        Stage::Latency => "Idle latency".into(),
        stage => format!("Loaded latency · {}", label(stage)),
    }
}

pub fn direction_label(stage: Stage, direction: Direction) -> String {
    match stage {
        Stage::Bidirectional => format!("Bi-dir {}", arrow(direction)),
        stage => label(stage).into(),
    }
}

pub fn arrow(direction: Direction) -> &'static str {
    if direction == Direction::Down { "↓" } else { "↑" }
}

pub fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Complete => "Complete",
        Outcome::Partial => "Partial",
        Outcome::Incomplete => "Incomplete",
        Outcome::Stopped => "Stopped",
        Outcome::Failed => "Failed",
    }
}

pub fn clock(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

pub fn ms(duration: Duration) -> String {
    format!("{} ms", format::latency(duration.as_secs_f64() * 1e3))
}

/// Thousands separated by commas.
pub fn count(value: usize) -> String {
    let digits = value.to_string();
    let groups: Vec<_> = digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(String::from_utf8_lossy)
        .collect();
    groups.join(",")
}

/// A setting's duration: milliseconds, seconds, then whole minutes or hours with what remains.
pub fn setting(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        return format!("{} ms", duration.as_millis());
    }
    if duration < Duration::from_secs(60) {
        return format!("{} s", duration.as_secs_f64());
    }
    let (step, (large, small)) = match duration < Duration::from_secs(3600) {
        true => (1.0, ("min", "s")),
        false => (60.0, ("h", "min")),
    };
    let steps = (duration.as_secs_f64() / step).round() as u64;
    match steps % 60 {
        0 => format!("{} {large}", steps / 60),
        rest => format!("{} {large} {rest} {small}", steps / 60),
    }
}

/// An HTTP version; none is automatic.
pub fn protocol(protocol: Option<Protocol>) -> &'static str {
    match protocol {
        None => "Automatic",
        Some(Protocol::Http1) => "HTTP/1.1",
        Some(Protocol::Http2) => "HTTP/2",
        Some(Protocol::Http3) => "HTTP/3",
        Some(Protocol::Negotiated) => "Negotiated",
    }
}

pub fn throughput_transport(transport: ThroughputTransport) -> &'static str {
    match transport {
        ThroughputTransport::FetchStream => "Fetch streams",
        ThroughputTransport::WebTransport => "WebTransport streams",
        ThroughputTransport::WebTransportDatagram => "WebTransport datagrams",
    }
}

pub fn latency_transport(transport: LatencyTransport) -> &'static str {
    match transport {
        LatencyTransport::WebSocket => "WebSocket",
        LatencyTransport::WebTransport => "WebTransport datagrams",
    }
}

/// The HTTP version a latency transport runs over.
pub fn latency_protocol(transport: LatencyTransport) -> Protocol {
    match transport {
        LatencyTransport::WebSocket => Protocol::Http1,
        LatencyTransport::WebTransport => Protocol::Http3,
    }
}

/// A path as its transport, HTTP version and security.
pub fn connection(transport: &str, version: Protocol, origin: &Origin) -> String {
    let security = if origin.scheme == Scheme::Https { "TLS" } else { "clear" };
    format!("{transport} · {} · {security}", protocol(Some(version)))
}

pub fn throughput_path(path: &ThroughputPath) -> String {
    connection(throughput_transport(path.transport), path.protocol, &path.origin)
}

pub fn latency_path(path: &LatencyPath) -> String {
    connection(latency_transport(path.transport), latency_protocol(path.transport), &path.origin)
}

/// A server's name and, when it has one, its location.
pub fn server(name: &str, location: &str) -> String {
    match location {
        "" => name.to_owned(),
        location => format!("{name} · {location}"),
    }
}

/// The cadence presets setup steps through, with their names.
pub const CADENCES: [(Cadence, &str); 4] = [
    (Cadence::ReplyDriven, "Reply-driven"),
    (Cadence::Every(Duration::from_millis(80)), "Fast (80 ms)"),
    (Cadence::Every(Duration::from_millis(250)), "Medium (250 ms)"),
    (Cadence::Every(Duration::from_millis(600)), "Slow (600 ms)"),
];

/// A probe cadence by its preset's name, or its spacing.
pub fn cadence(cadence: Cadence) -> String {
    match (CADENCES.iter().find(|(preset, _)| *preset == cadence), cadence) {
        (None, Cadence::Every(spacing)) => format!("Custom ({})", setting(spacing)),
        (preset, _) => preset.unwrap_or(&CADENCES[0]).1.into(),
    }
}
