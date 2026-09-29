use graphite_meter_core::discovery::{Capabilities, LatencyTarget, ThroughputTarget};
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
    /// The probes in a row that timed out, as Go's live view counts them.
    pub timeouts: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ServerSummary {
    pub id: String,
    pub name: String,
    pub location: String,
    pub origin: String,
    pub throughput: Option<ThroughputTarget>,
    pub latency: Option<LatencyTarget>,
    /// The paths the server's discovery advertised, kept when its check failed after discovery,
    /// as Go keeps a preparation error's preflight.
    pub offered: Option<Capabilities>,
    pub error: Option<String>,
    /// The check stopped at a sign-in the server requires, which Go shows as Sign in, not Failed.
    pub sign_in: bool,
}

impl ServerSummary {
    pub fn checked(&self) -> bool {
        self.throughput.is_some() || self.latency.is_some()
    }

    pub fn has_check_result(&self) -> bool {
        self.checked() || self.error.is_some()
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

    /// Records a server's first failure in a stage and scope, for `reason`; `at` is the time since
    /// the run started.
    pub fn failure(
        &mut self,
        id: &str,
        scope: FailureScope,
        reason: graphite_meter_core::failure::FailureReason,
        at: Duration,
    ) {
        let Some(stage) = self.stage else {
            return;
        };
        if self
            .failures
            .iter()
            .any(|failure| failure.server_id == id && failure.stage == stage && failure.scope == scope)
        {
            return;
        }
        self.failures.push(ServerFailure {
            server_id: id.into(),
            stage,
            scope,
            reason,
            at,
        });
    }

    /// A stage opens for the servers `ids`, with no samples yet.
    pub(crate) fn open_stage(&mut self, stage: Stage, ids: impl Iterator<Item = String>) {
        (self.phase, self.stage, self.latest) = (Phase::Preparing, Some(stage), Point::default());
        let latency = |id| ServerLatency {
            id,
            ..ServerLatency::default()
        };
        self.server_latencies = ids.map(latency).collect();
    }
}
