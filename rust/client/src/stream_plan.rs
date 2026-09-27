//! Stage-wide lane allocation, resolved before starting transfer requests.
use crate::{Error, config::Config, model::Stage};
use graphite_meter_core::discovery::{LatencyTarget, Protocol, ThroughputTarget, ThroughputTransport};
use std::collections::{BTreeMap, HashSet};

pub const MAX_STREAMS: usize = 14;

/// Include only latency sessions that the stage will actually start.
pub struct Participant<'a> {
    pub id: &'a str,
    pub throughput: Option<&'a ThroughputTarget>,
    pub latency: Option<&'a LatencyTarget>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaneCounts {
    pub download: usize,
    pub upload: usize,
}

pub struct StageLanePlan {
    participants: BTreeMap<String, LaneCounts>,
}

#[derive(Clone, Copy)]
enum Direction {
    Download,
    Upload,
}
impl StageLanePlan {
    pub fn new(config: &Config, stage: Stage, participants: &[Participant<'_>]) -> Result<Self, Error> {
        config.validate()?;
        let mut ids = HashSet::new();
        if participants.is_empty()
            || participants.len() > 4
            || participants
                .iter()
                .any(|participant| participant.id.is_empty() || !ids.insert(participant.id))
        {
            return Err("a stage requires one to four distinct participants".into());
        }
        let mut counts = BTreeMap::new();
        for participant in participants {
            let mut assigned = LaneCounts::default();
            if stage.downloads() || stage.uploads() {
                let target = participant.throughput.ok_or("missing throughput target")?;
                assigned.download = if stage.downloads() {
                    desired(config, target, Direction::Download)
                } else {
                    0
                };
                assigned.upload = if stage.uploads() {
                    desired(config, target, Direction::Upload)
                } else {
                    0
                };
            }
            counts.insert(participant.id.to_owned(), assigned);
        }
        Ok(Self { participants: counts })
    }

    pub fn lanes(&self, id: &str) -> Option<LaneCounts> {
        self.participants.get(id).copied()
    }
}

fn desired(config: &Config, target: &ThroughputTarget, direction: Direction) -> usize {
    if target.transport == ThroughputTransport::WebTransport {
        return if config.streams > 0 { config.streams } else { 1 };
    }
    if config.streams > 0 {
        return config.streams;
    }
    match (target.protocol, direction) {
        (Protocol::Http2, Direction::Download) | (Protocol::Http3, _) => 1,
        (Protocol::Http2, Direction::Upload) => 4,
        _ => config.auto_streams,
    }
}
