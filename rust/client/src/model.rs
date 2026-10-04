use graphite_meter_core::{
    discovery::{Capabilities, LatencyTarget, ThroughputTarget},
    failure::FailureReason,
    measurement::{self as core, AggregationInterval, MeasurementResult},
};
use std::{collections::VecDeque, time::Duration};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    #[default]
    Latency,
    Download,
    Upload,
    Bidirectional,
}

impl From<core::Stage> for Stage {
    fn from(stage: core::Stage) -> Self {
        match stage {
            core::Stage::Download => Self::Download,
            core::Stage::Upload => Self::Upload,
            core::Stage::Bidirectional => Self::Bidirectional,
        }
    }
}

impl Stage {
    pub fn name(self) -> &'static str {
        ["Latency", "Download", "Upload", "Bidirectional"][self as usize]
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
    pub down: Option<MeasurementResult>,
    pub up: Option<MeasurementResult>,
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
        ["Complete", "Partial", "Failed", "Stopped", "Skipped"][self as usize]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Stopped,
    Failed(FailureReason),
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
    pub reason: FailureReason,
    pub at: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct ServerContribution {
    pub id: String,
    pub down: Option<MeasurementResult>,
    pub up: Option<MeasurementResult>,
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
    /// Since the last sample, the measured probes as Go's chart takes each one: every 50 ms step's
    /// start, mean round trip in milliseconds and replies, with a timeout as a NaN step.
    pub steps: Vec<(tokio::time::Instant, f64, usize)>,
}

impl ServerLatency {
    /// Adds a measured reply's round trip in milliseconds, or NaN for a timeout, to `steps`.
    pub(crate) fn step(steps: &mut Vec<(tokio::time::Instant, f64, usize)>, at: tokio::time::Instant, ms: f64) {
        match steps.last_mut() {
            Some((start, mean, count)) if at < *start + Duration::from_millis(50) && !(ms + *mean).is_nan() => {
                *count += 1;
                *mean += (ms - *mean) / *count as f64;
            }
            _ => steps.push((at, ms, 1)),
        }
    }
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
    pub fn has_check_result(&self) -> bool {
        self.throughput.is_some() || self.latency.is_some() || self.error.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthPrompt {
    pub deadline: tokio::time::Instant,
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
    pub intervals: VecDeque<AggregationInterval>,
    pub omitted_intervals: usize,
    pub participants: Vec<String>,
    pub latency_focus: Option<String>,
    pub plan: Vec<Stage>,
    pub duration: Duration,
}

impl Snapshot {
    /// Failed when a planned result is missing (a present focus's median included), partial after a failure.
    pub fn stage_status(&self, result: &StageResult) -> StageStatus {
        let mut hosts = result.server_latencies.iter();
        let focus = hosts.find(|host| Some(&host.id) == self.latency_focus.as_ref());
        let unfocused = focus.is_none_or(|host| host.median().is_none() || !self.participants.contains(&host.id));
        if result.stopped {
            StageStatus::Stopped
        } else if result.lacks_throughput() || result.stage == Stage::Latency && unfocused {
            StageStatus::Failed
        } else if self.failures.iter().any(|failure| failure.stage == result.stage) {
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
        let mut results = self.results.iter();
        let missing = results.any(|result| self.stage_status(result) == StageStatus::Failed);
        self.phase = match () {
            _ if stopped => Phase::Cancelled,
            _ if missing => Phase::Incomplete,
            _ if !self.failures.is_empty() => Phase::Partial,
            _ => Phase::Complete,
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
        let focus = self.latency_focus.as_ref();
        if focus.is_some_and(|focus| self.participants.contains(focus)) {
            return;
        }
        let idle = self.results.iter().find(|result| result.stage == Stage::Latency);
        let hosts = idle.map_or(&[][..], |idle| &idle.server_latencies);
        let measured = |id: &&String| hosts.iter().any(|host| host.id == **id && host.median().is_some());
        if let Some(survivor) = self.participants.iter().find(measured) {
            self.latency_focus = Some(survivor.clone());
        }
    }

    /// Records a server's first failure in the stage and scope; `at` is the time since the run started.
    pub fn failure(&mut self, id: &str, scope: FailureScope, reason: FailureReason, at: Duration) {
        let Some(stage) = self.stage else { return };
        let known =
            |failure: &ServerFailure| failure.server_id == id && failure.stage == stage && failure.scope == scope;
        if !self.failures.iter().any(known) {
            let failure = ServerFailure { server_id: id.into(), stage, scope, reason, at };
            self.failures.push(failure);
        }
    }

    /// A stage opens for the servers `ids`, with no samples yet.
    pub(crate) fn open_stage(&mut self, stage: Stage, ids: impl Iterator<Item = String>) {
        (self.phase, self.stage, self.latest) = (Phase::Preparing, Some(stage), Point::default());
        let latency = |id| ServerLatency { id, ..ServerLatency::default() };
        self.server_latencies = ids.map(latency).collect();
    }
}
