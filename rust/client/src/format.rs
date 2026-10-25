//! Every figure, unit and label the report and the interface print (`api/format.testvectors.json`).
use crate::{
    config::{Config, preset},
    model::{Cadence, Direction, Outcome, Scope, Stage, StageStatus},
    net::{LatencyPath, ThroughputPath},
};
use graphite_meter_proto::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    origin::{Origin, Scheme},
};
use std::time::Duration;

pub const MISSING: &str = "–";
pub const UNREACHED: &str = "Not reached";

/// `value` with `places` decimals, an exact tie rounded up as the browser's `toFixed` does, not to even.
fn fixed(value: f64, places: usize) -> String {
    let scale = 10_f64.powi(places as i32);
    format!("{:.places$}", (value * scale).round() / scale)
}

/// Milliseconds: one decimal below 100, whole from there.
pub fn ms(value: f64) -> String {
    let decimals = if (value * 10.0).round().abs() < 1000.0 {
        1
    } else {
        0
    };
    fixed(value, decimals)
}

/// A round trip in milliseconds; below 0.1 shows as `< 0.1`.
pub fn latency(value: f64) -> String {
    if (0.0..0.1).contains(&value) {
        "< 0.1".into()
    } else {
        ms(value)
    }
}

pub fn round_trip(duration: Duration) -> String {
    format!("{} ms", latency(duration.as_secs_f64() * 1e3))
}

/// Fastest to slowest, both ends even when they read the same.
pub fn latency_range(fastest: Duration, slowest: Duration) -> String {
    let ms = |duration: Duration| latency(duration.as_secs_f64() * 1e3);
    format!("{} to {} ms", ms(fastest), ms(slowest))
}

/// Added latency in milliseconds, always signed; what rounds to zero is `+0.0`.
pub fn added(value: f64) -> String {
    let sign = if (value * 10.0).round() < 0.0 {
        '−'
    } else {
        '+'
    };
    format!("{sign}{}", ms(value.abs()))
}

/// A speed in its unit: two decimals below 100, one below 1000, whole from there.
pub fn speed(value: f64) -> String {
    match () {
        _ if (value * 10.0).round() >= 10_000.0 => fixed(value, 0),
        _ if (value * 100.0).round() >= 10_000.0 => fixed(value, 1),
        _ => fixed(value, 2),
    }
}

/// Bytes per second as bits per second, in the largest unit the value reaches 1.2 of.
pub fn rate(bytes_per_sec: f64) -> String {
    let (value, unit) = tier(bytes_per_sec * 8.0, 1.2);
    format!("{} {unit}", speed(value))
}

/// Bits per second in the largest unit the value reaches `headroom` of, and that unit.
pub fn tier(bits: f64, headroom: f64) -> (f64, &'static str) {
    const UNITS: [&str; 5] = ["bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"];
    let step = |tier: usize| 1e3_f64.powi(tier as i32);
    let tier = (1..UNITS.len())
        .take_while(|&tier| bits >= headroom * step(tier))
        .count();
    (bits / step(tier), UNITS[tier])
}

/// A byte count: whole bytes below 999.5, one decimal in decimal units from there.
pub fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = count as f64;
    let mut tier = 0;
    while tier + 1 < UNITS.len() && (value * 10.0).round() >= 10_000.0 {
        value /= 1000.0;
        tier += 1;
    }
    match tier {
        0 => format!("{count} B"),
        _ => format!("{} {}", fixed(value, 1), UNITS[tier]),
    }
}

/// A share in percent: two decimals below 1%, so a rare timeout stays legible, one from there.
pub fn percent(ratio: f64) -> String {
    let places = if ratio > 0.0 && ratio < 0.01 { 2 } else { 1 };
    format!("{}%", fixed(ratio * 100.0, places))
}

/// A dial or axis label: decimals down to the third significant figure, no trailing zeros.
pub fn tick(value: f64) -> String {
    if value == 0.0 || value.is_nan() {
        return "0".into();
    }
    let places = (2 - value.abs().log10().floor() as i32).clamp(0, 100) as usize;
    let text = fixed(value, places);
    match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.').into(),
        false => text,
    }
}

pub fn count(value: usize) -> String {
    let digits = value.to_string();
    let groups = digits.as_bytes().rchunks(3).rev();
    let groups: Vec<_> = groups.map(String::from_utf8_lossy).collect();
    groups.join(",")
}

/// Seconds with one decimal below a minute, then minutes and whole seconds, then hours and whole minutes.
pub fn duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if (seconds * 10.0).round() < 600.0 {
        return format!("{} s", fixed(seconds, 1));
    }
    let pair = |large: u64, unit: &str, small: u64, small_unit: &str| match small {
        0 => format!("{large} {unit}"),
        small => format!("{large} {unit} {small} {small_unit}"),
    };
    let whole = seconds.round() as u64;
    if whole < 3600 {
        return pair(whole / 60, "min", whole % 60, "s");
    }
    let minutes = (whole as f64 / 60.0).round() as u64;
    pair(minutes / 60, "h", minutes % 60, "min")
}

/// A planned time: as `duration`, without a whole second's decimal.
pub fn stage_time(planned: Duration) -> String {
    let seconds = planned.as_secs_f64();
    match (seconds * 10.0).round() < 600.0 {
        true => format!("{} s", fixed(seconds, 1).trim_end_matches(".0")),
        false => duration(planned),
    }
}

/// Seconds into a stage over its planned time; the seconds keep counting past a minute.
pub fn progress(elapsed: Duration, planned: Duration) -> String {
    format!(
        "{} / {}",
        fixed(elapsed.as_secs_f64(), 1),
        stage_time(planned)
    )
}

pub fn label(stage: Stage) -> &'static str {
    match stage {
        Stage::Latency => "Latency",
        Stage::Download => "Download",
        Stage::Upload => "Upload",
        Stage::Bidirectional => "Bidirectional",
    }
}

pub fn population(stage: Stage) -> &'static str {
    match stage {
        Stage::Latency => "Idle",
        Stage::Download => "Loaded download",
        Stage::Upload => "Loaded upload",
        Stage::Bidirectional => "Loaded bidirectional",
    }
}

pub fn place(stage: Stage, scope: Scope) -> &'static str {
    match scope {
        Scope::Server => "Start",
        Scope::Latency if stage != Stage::Latency => population(stage),
        _ => label(stage),
    }
}

/// `population` where a lane's label column cannot hold it.
pub fn compact_population(stage: Stage) -> &'static str {
    match stage {
        Stage::Latency => "Idle",
        Stage::Download => "Loaded down",
        Stage::Upload => "Loaded up",
        Stage::Bidirectional => "Loaded bi-dir",
    }
}

pub fn population_label(stage: Stage) -> String {
    match stage {
        Stage::Latency => "Idle latency".into(),
        stage => format!("Loaded latency, {}", label(stage)),
    }
}

pub fn direction_label(stage: Stage, direction: Direction) -> String {
    match stage {
        Stage::Bidirectional => format!("Bi-dir {}", arrow(direction)),
        stage => label(stage).into(),
    }
}

pub fn arrow(direction: Direction) -> &'static str {
    if direction == Direction::Down {
        "↓"
    } else {
        "↑"
    }
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

pub fn status_label(status: StageStatus) -> &'static str {
    match status {
        StageStatus::Complete => "Complete",
        StageStatus::Partial => "Partial",
        StageStatus::Failed => "Failed",
        StageStatus::Stopped => "Stopped",
    }
}

/// An HTTP version; none is automatic.
pub fn protocol(protocol: Option<Protocol>) -> &'static str {
    match protocol {
        None => "Automatic",
        Some(Protocol::Http1) => "HTTP/1.1",
        Some(Protocol::Http2) => "HTTP/2",
        Some(Protocol::Http3) => "HTTP/3",
        Some(Protocol::Negotiated) => "Negotiated HTTP",
    }
}

pub fn throughput_transport(transport: ThroughputTransport) -> &'static str {
    match transport {
        ThroughputTransport::FetchStream => "Fetch streams",
        ThroughputTransport::WebTransport => "WebTransport streams",
        ThroughputTransport::WebTransportDatagram => "Datagram flood, no streams",
    }
}

pub fn latency_transport(transport: LatencyTransport) -> &'static str {
    match transport {
        LatencyTransport::WebSocket => "WebSocket",
        LatencyTransport::WebTransport => "WebTransport datagrams",
    }
}

pub fn latency_protocol(transport: LatencyTransport) -> Protocol {
    match transport {
        LatencyTransport::WebSocket => Protocol::Http1,
        LatencyTransport::WebTransport => Protocol::Http3,
    }
}

/// A path as its transport over its HTTP version, and whether TLS carries it.
pub fn path(transport: &str, version: Protocol, origin: &Origin) -> String {
    let tls = if origin.scheme == Scheme::Https {
        " (TLS)"
    } else {
        ""
    };
    format!("{transport} over {}{tls}", protocol(Some(version)))
}

pub fn throughput_path(path: &ThroughputPath) -> String {
    self::path(
        throughput_transport(path.transport),
        path.protocol,
        &path.origin,
    )
}

pub fn latency_path(path: &LatencyPath) -> String {
    self::path(
        latency_transport(path.transport),
        latency_protocol(path.transport),
        &path.origin,
    )
}

pub fn streams(config: &Config, path: &ThroughputPath) -> String {
    let lanes = config.lanes(path.protocol, path.transport);
    match (config.streams.forced, path.transport, path.protocol) {
        (Some(forced), ..) => format!("Exact · {forced} per direction"),
        (_, ThroughputTransport::FetchStream, Protocol::Http1) => {
            format!("Automatic · up to {} per direction", lanes.down)
        }
        (_, ThroughputTransport::FetchStream, Protocol::Http2 | Protocol::Http3) => {
            format!("Automatic · {} download, {} upload", lanes.down, lanes.up)
        }
        (_, ThroughputTransport::FetchStream, _) => "Automatic".into(),
        _ => "Automatic · 1 continuous stream per direction".into(),
    }
}

/// A server by name and location, a location the name already holds read once.
pub fn server(name: &str, location: &str) -> String {
    let (lower_name, lower_location) = (name.to_lowercase(), location.to_lowercase());
    match () {
        _ if lower_location.is_empty() || lower_name.contains(&lower_location) => name.to_owned(),
        _ if lower_location.starts_with(&lower_name) => location.to_owned(),
        _ => format!("{name}, {location}"),
    }
}

pub fn authority(origin: &Origin) -> String {
    let text = origin.to_string();
    text.split_once("://")
        .map_or_default(|(_, authority)| authority.to_owned())
}

/// A probe cadence by its preset's name, or its spacing.
pub fn cadence(cadence: Cadence) -> String {
    let spacing = |spacing: Duration| match spacing < Duration::from_secs(1) {
        true => format!("{} ms", spacing.as_millis()),
        false => stage_time(spacing),
    };
    let name = preset(cadence).map_or_else(|| "Custom".into(), crate::text::capitalized);
    match cadence {
        Cadence::ReplyDriven => name,
        Cadence::Every(every) => format!("{name} ({})", spacing(every)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_location_the_name_holds_reads_once() {
        assert_eq!(server("Nuremberg", "Nuremberg, DE"), "Nuremberg, DE");
        assert_eq!(server("Meter Nuremberg", "Nuremberg"), "Meter Nuremberg");
        assert_eq!(server("Meter", "Berlin"), "Meter, Berlin");
        assert_eq!(server("Meter", ""), "Meter");
    }
}
