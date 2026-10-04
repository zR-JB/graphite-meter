use crate::{Error, model::Stage};
use graphite_meter_core::discovery::{
    LatencyTransport, MAX_STAGE_LIMIT, MIN_STAGE_LIMIT, Protocol, ThroughputTarget, ThroughputTransport,
};
use std::{ops::RangeInclusive, time::Duration};

pub const MAX_STREAMS: usize = 14;
pub(crate) const STAGE_BOUND: RangeInclusive<Duration> = MIN_STAGE_LIMIT..=MAX_STAGE_LIMIT;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub url: String,
    pub servers: Vec<String>,
    pub throughput_origin: Option<String>,
    pub throughput_protocol: Option<Protocol>,
    pub throughput_transport: Option<ThroughputTransport>,
    pub latency_origin: Option<String>,
    pub latency_transport: Option<LatencyTransport>,
    pub stages: Vec<Stage>,
    pub warmup: Duration,
    pub latency_duration: Duration,
    pub download_duration: Duration,
    pub upload_duration: Duration,
    pub bidirectional_duration: Duration,
    pub auto_streams: usize,
    pub streams: usize,
    pub ping_interval: Duration,
    pub loaded_ping_interval: Duration,
    pub loaded_latency: bool,
    pub insecure: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:7246".into(),
            servers: Vec::new(),
            throughput_origin: None,
            throughput_protocol: None,
            throughput_transport: None,
            latency_origin: None,
            latency_transport: None,
            stages: vec![Stage::Latency, Stage::Download, Stage::Upload],
            warmup: Duration::from_millis(800),
            latency_duration: Duration::from_secs(4),
            download_duration: Duration::from_secs(10),
            upload_duration: Duration::from_secs(10),
            bidirectional_duration: Duration::from_secs(10),
            auto_streams: 6,
            streams: 0,
            ping_interval: Duration::ZERO,
            loaded_ping_interval: Duration::from_millis(250),
            loaded_latency: true,
            insecure: false,
        }
    }
}

impl Config {
    pub(crate) fn preparation_key(&self) -> Self {
        let mut key = self.clone();
        key.url = graphite_meter_core::origin::canonical_origin(&key.url).unwrap_or(key.url);
        key.servers.sort_unstable();
        // As Go's key, loaded latency counts only through the latency it needs.
        let latency = std::mem::take(&mut key.loaded_latency) || key.stages.contains(&Stage::Latency);
        let upload = key.stages.iter().any(|stage| stage.uploads());
        let transfer = key.stages.iter().any(|stage| stage.downloads() || stage.uploads());
        key.stages = vec![Stage::Latency, Stage::Download, Stage::Upload];
        key.stages.retain(|stage| [latency, transfer, upload][*stage as usize]);
        key.warmup = Duration::ZERO;
        key.latency_duration = Duration::ZERO;
        key.download_duration = Duration::ZERO;
        key.upload_duration = Duration::ZERO;
        key.bidirectional_duration = Duration::ZERO;
        key
    }

    pub fn duration(&self, stage: Stage) -> Duration {
        match stage {
            Stage::Latency => self.latency_duration,
            Stage::Download => self.download_duration,
            Stage::Upload => self.upload_duration,
            Stage::Bidirectional => self.bidirectional_duration,
        }
    }

    pub fn lanes(&self, target: &ThroughputTarget) -> (usize, usize) {
        match (self.streams, target.transport, target.protocol) {
            (0, ThroughputTransport::WebTransport, _) | (0, _, Protocol::Http3) => (1, 1),
            (0, _, Protocol::Http2) => (1, 4),
            (0, ..) => (self.auto_streams, self.auto_streams),
            (forced, ..) => (forced, forced),
        }
    }

    /// Go's `Config.Validate` in its order and words. Like Go's, it leaves origins to the path check,
    /// which refuses a malformed one, and server IDs to the catalogue's selection.
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_settings()?;
        self.validate_ceiling()
    }

    /// Go's Validate before its checkPaths, which refuses a flag's path choice before the cadence ceiling.
    pub(crate) fn validate_settings(&self) -> Result<(), Error> {
        if self.stages.is_empty() {
            return Err("select at least one stage: latency, download, upload or bidirectional".into());
        }
        if self.warmup > Duration::from_secs(4) {
            return Err("warmup must be from 0s to 4s".into());
        }
        // Go checks every stage's duration, including stages that are off.
        for stage in [Stage::Latency, Stage::Download, Stage::Upload, Stage::Bidirectional] {
            if !STAGE_BOUND.contains(&self.duration(stage)) {
                return Err(format!("{} duration must be from 1s to 24h", stage.name().to_lowercase()).into());
            }
        }
        let fast = |interval: Duration| !interval.is_zero() && interval < Duration::from_millis(80);
        if fast(self.ping_interval) || fast(self.loaded_ping_interval) {
            return Err("latency cadence must be reply-driven or at least 80ms".into());
        }
        if self.streams > MAX_STREAMS {
            return Err(format!(
                "forced streams must be from 1 to {MAX_STREAMS} per server and direction, or 0 for automatic"
            )
            .into());
        }
        if self.auto_streams == 0 || self.auto_streams > MAX_STREAMS {
            return Err(format!("the automatic stream maximum must be from 1 to {MAX_STREAMS} per direction").into());
        }
        Ok(())
    }

    /// The end of Go's checkPaths.
    pub(crate) fn validate_ceiling(&self) -> Result<(), Error> {
        if self.ping_interval.max(self.loaded_ping_interval) > Duration::from_secs(15) {
            return Err("latency interval must be at most 15s, half the server's 30s lane idle bound".into());
        }
        Ok(())
    }
}
