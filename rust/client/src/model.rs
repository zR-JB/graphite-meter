use graphite_meter_core::discovery::{LatencyTarget, LatencyTransport, Protocol, ThroughputTarget};
use graphite_meter_core::origin::target_origin;
use std::{collections::VecDeque, time::Duration};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    #[default]
    Latency,
    Download,
    Upload,
    Bidirectional,
}

impl From<graphite_meter_core::measurement::Stage> for Stage {
    fn from(stage: graphite_meter_core::measurement::Stage) -> Self {
        match stage {
            graphite_meter_core::measurement::Stage::Download => Self::Download,
            graphite_meter_core::measurement::Stage::Upload => Self::Upload,
            graphite_meter_core::measurement::Stage::Bidirectional => Self::Bidirectional,
        }
    }
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
    Checking,
    Preparing,
    Warmup,
    Measuring,
    Complete,
    Partial,
    Incomplete,
    Cancelled,
    Failed,
}

impl Phase {
    pub fn live(self) -> bool {
        matches!(self, Self::Preparing | Self::Warmup | Self::Measuring)
    }
    pub fn busy(self) -> bool {
        self == Self::Checking || self.live()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Point {
    pub elapsed: Duration,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub latency_ms: Option<f64>,
    pub sample_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct StageResult {
    pub stage: Stage,
    pub elapsed: Duration,
    pub down: Option<graphite_meter_core::measurement::MeasurementResult>,
    pub up: Option<graphite_meter_core::measurement::MeasurementResult>,
    pub stopped: bool,
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
    /// A direction the stage measures has no rate.
    pub fn lacks_throughput(&self) -> bool {
        self.stage.downloads() && self.down_bps().is_none() || self.stage.uploads() && self.up_bps().is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageStatus {
    Complete,
    Partial,
    Failed,
    Stopped,
    Skipped,
}

impl StageStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Complete => "Complete",
            Self::Partial => "Partial",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Skipped => "Skipped",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Stopped,
    Failed(graphite_meter_core::failure::FailureReason),
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
    pub at: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct ServerContribution {
    pub id: String,
    pub down: Option<graphite_meter_core::measurement::MeasurementResult>,
    pub up: Option<graphite_meter_core::measurement::MeasurementResult>,
}

#[derive(Clone, Debug, Default)]
pub struct ServerLatencyResult {
    pub elapsed: Option<Duration>,
    pub id: String,
    pub summary: graphite_meter_core::latency::LatencySummary,
    pub ending: Option<Ending>,
}

impl ServerLatencyResult {
    pub fn median(&self) -> Option<u64> {
        if self.ending.is_some() && self.summary.count + self.summary.timeouts < 3 {
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
    pub latest: Point,
    pub history: Trace,
    pub results: Vec<StageResult>,
    pub servers: Vec<ServerSummary>,
    pub error: Option<String>,
    pub auth: Option<AuthPrompt>,
    pub server_latencies: Vec<ServerLatency>,
    pub failures: Vec<ServerFailure>,
    /// The run's aggregation intervals as Go's run details hold them: timed from the run's start,
    /// the latest 128 of the run, and how many older ones were dropped.
    pub intervals: VecDeque<graphite_meter_core::measurement::AggregationInterval>,
    pub omitted_intervals: usize,
    pub participants: Vec<String>,
    pub latency_focus: Option<String>,
    pub plan: Vec<Stage>,
    pub duration: Duration,
}

impl Snapshot {
    /// Failed when a planned result is missing (a present focus's median included), partial after a failure.
    pub fn stage_status(&self, result: &StageResult) -> StageStatus {
        let stage = result.stage;
        if result.stopped {
            StageStatus::Stopped
        } else if result.lacks_throughput()
            || stage == Stage::Latency
                && result
                    .server_latencies
                    .iter()
                    .find(|host| Some(&host.id) == self.latency_focus.as_ref())
                    .is_none_or(|host| host.median().is_none() || !self.participants.contains(&host.id))
        {
            StageStatus::Failed
        } else if self.failures.iter().any(|failure| failure.stage == stage) {
            StageStatus::Partial
        } else {
            StageStatus::Complete
        }
    }

    /// A run of `plan` starts: the servers the check found, the phase, prompt and error stay.
    pub(crate) fn start_run(&mut self, plan: &[Stage]) {
        *self = Self {
            phase: self.phase,
            servers: std::mem::take(&mut self.servers),
            error: self.error.take(),
            auth: self.auth.take(),
            duration: self.duration,
            plan: plan.to_vec(),
            ..Self::default()
        };
    }

    /// A run ends: stopped, incomplete when a stage lacks a planned result, or partial after a failure.
    pub(crate) fn finish_run(&mut self, stopped: bool) {
        let missing = self
            .results
            .iter()
            .any(|result| self.stage_status(result) == StageStatus::Failed);
        self.phase = if stopped {
            Phase::Cancelled
        } else if missing {
            Phase::Incomplete
        } else if !self.failures.is_empty() {
            Phase::Partial
        } else {
            Phase::Complete
        };
    }

    pub fn measured(&self) -> bool {
        self.results.iter().any(|result| result.elapsed > Duration::ZERO)
    }

    /// A server joined the run or a stage ended, as Go's run details report.
    pub fn started(&self) -> bool {
        !self.participants.is_empty() || !self.results.is_empty()
    }

    pub(crate) fn leave(&mut self, id: &str) {
        self.participants.retain(|participant| participant != id);
        self.refocus();
    }

    pub(crate) fn refocus(&mut self) {
        if self
            .latency_focus
            .as_ref()
            .is_some_and(|focus| self.participants.contains(focus))
        {
            return;
        }
        let idle = self.results.iter().find(|result| result.stage == Stage::Latency);
        let survivor = self.participants.iter().find(|participant| {
            idle.is_some_and(|idle| {
                idle.server_latencies
                    .iter()
                    .any(|host| host.id == **participant && host.median().is_some())
            })
        });
        if let Some(survivor) = survivor {
            self.latency_focus = Some(survivor.clone());
        }
    }

    /// Records a server's first failure in a stage and scope; `at` is the time since the run started.
    pub fn failure(
        &mut self,
        id: &str,
        scope: FailureScope,
        error: &crate::Error,
        at: Duration,
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
            at,
        });
        Some(reason)
    }

    pub fn sample(&mut self, mut point: Point) {
        self.latest = point.clone();
        point.elapsed += self.offset();
        self.history.add(point);
    }

    /// The time the run's recorded stages took, where the current stage's trace points start.
    pub(crate) fn offset(&self) -> Duration {
        self.results.iter().map(|result| result.elapsed).sum()
    }

    /// A stage opens for the servers `ids`: each keeps its latency trace, marked at the stage's start.
    pub(crate) fn open_stage(&mut self, stage: Stage, ids: impl Iterator<Item = String>) {
        let start = Point {
            elapsed: self.offset(),
            ..Point::default()
        };
        (self.phase, self.stage, self.latest) = (Phase::Preparing, Some(stage), Point::default());
        self.history.add(start.clone());
        let mut previous = std::mem::take(&mut self.server_latencies);
        self.server_latencies = ids
            .map(|id| {
                let earlier = previous.iter_mut().find(|host| host.id == id);
                let mut history = earlier
                    .map(|host| std::mem::take(&mut host.history))
                    .unwrap_or_default();
                history.add(start.clone());
                ServerLatency {
                    id,
                    history,
                    ..ServerLatency::default()
                }
            })
            .collect();
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
