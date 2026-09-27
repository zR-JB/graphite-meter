use graphite_meter_core::discovery::{LatencyTarget, LatencyTransport, Protocol, ThroughputTarget};
use graphite_meter_core::origin::target_origin;
use std::{collections::VecDeque, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Latency,
    Download,
    Upload,
    Bidirectional,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Latency => crate::vocabulary::LATENCY.label,
            Self::Download => crate::vocabulary::DOWNLOAD.label,
            Self::Upload => crate::vocabulary::UPLOAD.label,
            Self::Bidirectional => crate::vocabulary::BIDIRECTIONAL.label,
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
    Incomplete,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct Point {
    pub elapsed: Duration,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub latency_ms: Option<f64>,
    pub sample_count: usize,
}

#[derive(Clone, Debug)]
pub struct StageResult {
    pub stage: Stage,
    pub elapsed: Duration,
    pub down: Option<graphite_meter_core::measurement::MeasurementResult>,
    pub up: Option<graphite_meter_core::measurement::MeasurementResult>,
    pub intervals: VecDeque<graphite_meter_core::measurement::AggregationInterval>,
    pub omitted_intervals: usize,
    pub complete: bool,
    pub server_latencies: Vec<ServerLatencyResult>,
    pub server_results: Vec<ServerContribution>,
}

impl StageResult {
    pub fn down_bps(&self) -> Option<f64> {
        self.down.as_ref()?.mean_bytes_per_sec.map(|rate| rate * 8.0)
    }
    pub fn up_bps(&self) -> Option<f64> {
        self.up.as_ref()?.mean_bytes_per_sec.map(|rate| rate * 8.0)
    }
    pub fn down_bytes(&self) -> u64 {
        self.down.as_ref().map_or(0, |result| result.total_bytes)
    }
    pub fn up_bytes(&self) -> u64 {
        self.up.as_ref().map_or(0, |result| result.total_bytes)
    }
    pub fn status(&self) -> StageStatus {
        let measured = (!self.stage.downloads() || self.down_bps().is_some())
            && (!self.stage.uploads() || self.up_bps().is_some())
            && (self.stage != Stage::Latency || self.server_latencies.iter().any(|host| host.median().is_some()));
        match (measured, self.complete) {
            (false, _) => StageStatus::Failed,
            (true, false) => StageStatus::Partial,
            (true, true) => StageStatus::Complete,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageStatus {
    Complete,
    Partial,
    Failed,
}

impl StageStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Complete => "Complete",
            Self::Partial => "Partial",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureScope {
    Throughput,
    Latency,
}

#[derive(Clone, Debug)]
pub struct ServerFailure {
    pub server_id: String,
    pub stage: Stage,
    pub scope: FailureScope,
    pub reason: graphite_meter_core::failure::FailureReason,
    pub message: String,
    pub at: Duration,
}

#[derive(Clone, Debug)]
pub struct ServerContribution {
    pub id: String,
    pub down: Option<graphite_meter_core::measurement::MeasurementResult>,
    pub up: Option<graphite_meter_core::measurement::MeasurementResult>,
    pub error: Option<String>,
}

impl ServerContribution {
    pub fn down_bps(&self) -> Option<f64> {
        self.down.as_ref()?.mean_bytes_per_sec.map(|rate| rate * 8.0)
    }
    pub fn up_bps(&self) -> Option<f64> {
        self.up.as_ref()?.mean_bytes_per_sec.map(|rate| rate * 8.0)
    }
    pub fn down_bytes(&self) -> u64 {
        self.down.as_ref().map_or(0, |result| result.total_bytes)
    }
    pub fn up_bytes(&self) -> u64 {
        self.up.as_ref().map_or(0, |result| result.total_bytes)
    }
}

#[derive(Clone, Debug)]
pub struct ServerLatencyResult {
    pub elapsed: Option<Duration>,
    pub id: String,
    pub summary: graphite_meter_core::latency::LatencySummary,
    pub error: Option<String>,
}

impl ServerLatencyResult {
    pub fn median(&self) -> Option<u64> {
        if self.error.is_some() && self.summary.count + self.summary.timeouts < 3 {
            return None;
        }
        self.summary.distribution.map(|distribution| distribution.p50)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ServerLatency {
    pub id: String,
    pub latest_ms: Option<f64>,
    pub history: Trace,
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
        let transport = crate::vocabulary::throughput_transport(Some(target.transport)).label;
        let protocol = crate::vocabulary::protocol(Some(target.protocol)).label;
        Some(format!("{transport} · {protocol} · {}", security(&target.base_url)))
    }

    pub fn latency_label(&self) -> Option<String> {
        let target = self.latency.as_ref()?;
        let transport = crate::vocabulary::latency_transport(Some(target.transport)).label;
        let protocol = crate::vocabulary::protocol(Some(if target.transport == LatencyTransport::WebSocket {
            Protocol::Http1
        } else {
            Protocol::Http3
        }))
        .label;
        Some(format!("{transport} · {protocol} · {}", security(&target.base_url)))
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
    pub deadline: tokio::time::Instant,
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
    pub history: Trace,
    pub results: Vec<StageResult>,
    pub servers: Vec<ServerSummary>,
    pub error: Option<String>,
    pub auth: Option<AuthPrompt>,
    pub server_latencies: Vec<ServerLatency>,
    pub failures: Vec<ServerFailure>,
    pub participants: Vec<String>,
    pub latency_focus: Option<String>,
}

impl Snapshot {
    pub(crate) fn leave(&mut self, id: &str) {
        self.participants.retain(|participant| participant != id);
        if self.latency_focus.as_deref() != Some(id) {
            return;
        }
        let idle = self.results.iter().find(|result| result.stage == Stage::Latency);
        let measured = |participant: &&String| {
            idle.is_some_and(|idle| {
                idle.server_latencies
                    .iter()
                    .any(|host| host.id == **participant && host.median().is_some())
            })
        };
        let focus = self.participants.iter().find(measured).or(self.participants.first());
        self.latency_focus = focus.cloned();
    }

    pub fn added_ms(&self, loaded: &StageResult, id: &str) -> Option<f64> {
        let median = |result: &StageResult| result.server_latencies.iter().find(|host| host.id == id)?.median();
        let idle = self.results.iter().find(|result| result.stage == Stage::Latency)?;
        if loaded.stage == Stage::Latency {
            return None;
        }
        Some((median(loaded)? as f64 - median(idle)? as f64) / 1e6)
    }

    pub fn failure(
        &mut self,
        id: &str,
        scope: FailureScope,
        error: &crate::Error,
    ) -> Option<graphite_meter_core::failure::FailureReason> {
        let stage = self.stage?;
        if self
            .failures
            .iter()
            .any(|failure| failure.server_id == id && failure.stage == stage && failure.scope == scope)
        {
            return None;
        }
        let reason = crate::failure::reason(error.as_ref(), self.phase != Phase::Measuring);
        self.failures.push(ServerFailure {
            server_id: id.into(),
            stage,
            scope,
            reason,
            message: error.to_string(),
            at: self.results.iter().map(|result| result.elapsed).sum::<Duration>() + self.latest.elapsed,
        });
        Some(reason)
    }

    pub fn sample(&mut self, mut point: Point) {
        self.latest = point.clone();
        point.elapsed += self.results.iter().map(|result| result.elapsed).sum::<Duration>();
        self.history.add(point);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Trace {
    pub points: VecDeque<Point>,
    step: Duration,
}

impl Trace {
    pub fn add(&mut self, mut point: Point) {
        point.sample_count = 1;
        self.step = self.step.max(Duration::from_millis(50));
        if let Some(last) = self.points.back_mut()
            && point.elapsed.saturating_sub(last.elapsed) < self.step
            && last.down_bps.is_some() == point.down_bps.is_some()
            && last.up_bps.is_some() == point.up_bps.is_some()
            && last.latency_ms.is_some() == point.latency_ms.is_some()
        {
            merge(last, point);
            return;
        }
        if self.points.len() == 480 {
            let mut coarsened = VecDeque::with_capacity(240);
            while let Some(mut first) = self.points.pop_front() {
                if let Some(second) = self.points.pop_front() {
                    merge(&mut first, second);
                }
                coarsened.push_back(first);
            }
            self.points = coarsened;
            self.step *= 2;
        }
        self.points.push_back(point);
    }
}

fn merge(first: &mut Point, second: Point) {
    let total = first.sample_count + second.sample_count;
    for (a, b) in [
        (&mut first.down_bps, second.down_bps),
        (&mut first.up_bps, second.up_bps),
        (&mut first.latency_ms, second.latency_ms),
    ] {
        *a = a
            .zip(b)
            .map(|(a, b)| (a * first.sample_count as f64 + b * second.sample_count as f64) / total as f64);
    }
    first.sample_count = total;
}
