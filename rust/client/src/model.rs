use graphite_meter_core::discovery::{
    LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport,
};
use graphite_meter_core::origin::target_origin;
use std::{collections::VecDeque, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Latency,
    Download,
    Upload,
    Bidirectional,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Latency => "Latency",
            Self::Download => "Download",
            Self::Upload => "Upload",
            Self::Bidirectional => "Bidirectional",
        }
    }
    pub fn downloads(self) -> bool {
        matches!(self, Self::Download | Self::Bidirectional)
    }
    pub fn uploads(self) -> bool {
        matches!(self, Self::Upload | Self::Bidirectional)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Setup,
    Preparing,
    Warmup,
    Measuring,
    Complete,
    Partial,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct Point {
    pub elapsed: Duration,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub latency_ms: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct StageResult {
    pub stage: Stage,
    pub elapsed: Duration,
    pub down_bytes: u64,
    pub up_bytes: u64,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub latency: graphite_meter_core::latency::LatencySummary,
    pub complete: bool,
    pub server_latencies: Vec<ServerLatencyResult>,
    pub server_results: Vec<ServerContribution>,
}

#[derive(Clone, Debug)]
pub struct ServerContribution {
    pub id: String,
    pub down_bytes: u64,
    pub up_bytes: u64,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ServerLatencyResult {
    pub id: String,
    pub summary: graphite_meter_core::latency::LatencySummary,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ServerLatency {
    pub id: String,
    pub latest_ms: Option<f64>,
    pub history: VecDeque<(Duration, Option<f64>)>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ServerSummary {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub throughput: Option<ThroughputTarget>,
    pub latency: Option<LatencyTarget>,
    pub error: Option<String>,
}

impl ServerSummary {
    pub fn checked(&self) -> bool {
        self.throughput.is_some() || self.latency.is_some()
    }

    pub fn has_check_result(&self) -> bool {
        self.checked() || self.error.is_some()
    }

    pub fn throughput_label(&self) -> Option<String> {
        let target = self.throughput.as_ref()?;
        let transport = match target.transport {
            ThroughputTransport::FetchStream => "Fetch stream",
            ThroughputTransport::WebTransport => "WebTransport stream",
            ThroughputTransport::WebTransportDatagram => "WebTransport datagram",
        };
        let protocol = match target.protocol {
            Protocol::Http1 => "HTTP/1.1",
            Protocol::Http2 => "HTTP/2",
            Protocol::Http3 => "HTTP/3",
            Protocol::Negotiated => "Negotiated",
        };
        Some(format!(
            "{transport} · {protocol} · {}",
            security(&target.base_url)
        ))
    }

    pub fn latency_label(&self) -> Option<String> {
        let target = self.latency.as_ref()?;
        let (transport, protocol) = match target.transport {
            LatencyTransport::WebSocket => ("WebSocket", "HTTP/1.1"),
            LatencyTransport::WebTransport => ("WebTransport", "HTTP/3"),
        };
        Some(format!(
            "{transport} · {protocol} · {}",
            security(&target.base_url)
        ))
    }

    pub fn connection_label(&self) -> String {
        [self.throughput_label(), self.latency_label()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

fn security(origin: &str) -> &'static str {
    match target_origin(origin) {
        Ok(Some(parsed)) if parsed.scheme == "https" => "TLS",
        Ok(Some(_)) => "clear",
        _ => "unknown",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthPrompt {
    pub origin: String,
    pub browser_url: String,
    pub code: String,
}

/// Latest display state; a slow terminal cannot backpressure measurement IO.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub phase: Phase,
    pub stage: Option<Stage>,
    pub status: String,
    pub latest: Point,
    pub history: VecDeque<Point>,
    pub results: Vec<StageResult>,
    pub servers: Vec<ServerSummary>,
    pub error: Option<String>,
    pub auth: Option<AuthPrompt>,
    pub server_latencies: Vec<ServerLatency>,
}

impl Snapshot {
    pub fn sample(&mut self, point: Point) {
        self.latest = point.clone();
        if self.history.len() == 300 {
            self.history.pop_front();
        }
        self.history.push_back(point);
    }
}
