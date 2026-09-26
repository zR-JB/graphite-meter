use crate::{Error, model::Stage};
use graphite_meter_core::discovery::{LatencyTransport, Protocol, ThroughputTransport};
use std::time::Duration;

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
            ping_interval: Duration::from_millis(250),
            loaded_latency: true,
            insecure: false,
        }
    }
}

impl Config {
    pub fn duration(&self, stage: Stage) -> Duration {
        match stage {
            Stage::Latency => self.latency_duration,
            Stage::Download => self.download_duration,
            Stage::Upload => self.upload_duration,
            Stage::Bidirectional => self.bidirectional_duration,
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        graphite_meter_core::origin::canonical_origin(&self.url)?;
        for origin in [&self.throughput_origin, &self.latency_origin]
            .into_iter()
            .flatten()
        {
            graphite_meter_core::origin::canonical_origin(origin)?;
        }
        if self.servers.len() > 4
            || self
                .servers
                .iter()
                .enumerate()
                .any(|(i, id)| id.is_empty() || self.servers[..i].contains(id))
        {
            return Err("select up to four different server IDs".into());
        }
        if self.stages.is_empty() {
            return Err("select at least one measurement stage".into());
        }
        if self.auto_streams == 0 || self.auto_streams > 128 || self.streams > 128 {
            return Err(
                "stream counts must be within 1..=128 (0 means automatic for --streams)".into(),
            );
        }
        if self.ping_interval.is_zero() {
            return Err("ping interval must be positive".into());
        }
        if self.latency_transport == Some(LatencyTransport::WebTransport)
            && self.ping_interval > Duration::from_secs(15)
        {
            return Err("WebTransport ping interval must not exceed 15 seconds".into());
        }
        for stage in &self.stages {
            if self.duration(*stage).is_zero() {
                return Err("stage duration must be positive".into());
            }
        }
        Ok(())
    }
}
