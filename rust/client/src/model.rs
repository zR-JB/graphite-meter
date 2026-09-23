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
    pub transport: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
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
