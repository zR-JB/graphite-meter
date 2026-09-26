//! Stage-wide lane allocation, resolved before starting transfer requests.
use crate::{Error, config::Config, model::Stage};
use graphite_meter_core::{
    discovery::{LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport},
    origin::canonical_origin,
};
use std::collections::{BTreeMap, HashSet};

const MAX_DIRECTION_LANES: usize = 128;
const HTTP1_CONNECTIONS: usize = 6;
const WT_LANES: usize = 16;

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
impl LaneCounts {
    fn count(&mut self, direction: Direction) -> &mut usize {
        match direction {
            Direction::Download => &mut self.download,
            Direction::Upload => &mut self.upload,
        }
    }
}

struct Lane<'a> {
    id: &'a str,
    direction: Direction,
    ceiling: usize,
}
#[derive(Default)]
struct OriginBudget<'a> {
    control: usize,
    lanes: Vec<Lane<'a>>,
}

impl StageLanePlan {
    pub fn new(
        config: &Config,
        stage: Stage,
        participants: &[Participant<'_>],
    ) -> Result<Self, Error> {
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
        let mut origins: BTreeMap<String, OriginBudget<'_>> = BTreeMap::new();
        for participant in participants {
            let mut assigned = LaneCounts::default();
            if stage.downloads() || stage.uploads() {
                let target = participant.throughput.ok_or("missing throughput target")?;
                let origin = canonical_origin(&target.base_url)?;
                let budget = origins.entry(origin).or_default();
                for direction in [Direction::Download, Direction::Upload] {
                    let active = match direction {
                        Direction::Download => stage.downloads(),
                        Direction::Upload => stage.uploads(),
                    };
                    if !active {
                        continue;
                    }
                    let ceiling = desired(config, target, direction);
                    *assigned.count(direction) = ceiling;
                    if target.transport == ThroughputTransport::FetchStream {
                        if matches!(direction, Direction::Upload) {
                            budget.control += 1; // Receiver progress feed.
                        }
                        if matches!(target.protocol, Protocol::Http1 | Protocol::Negotiated) {
                            budget.lanes.push(Lane {
                                id: participant.id,
                                direction,
                                ceiling,
                            });
                        }
                    }
                }
                if config.loaded_latency
                    && let Some(latency) = participant.latency
                    && latency.transport == LatencyTransport::WebSocket
                {
                    origins
                        .entry(canonical_origin(&latency.base_url)?)
                        .or_default()
                        .control += 1;
                }
            }
            counts.insert(participant.id.to_owned(), assigned);
        }
        for (origin, budget) in origins {
            if budget.lanes.is_empty() {
                continue;
            }
            // Keep a connection available for upload checkpoint/finish requests.
            let reserved = budget.control + usize::from(stage.uploads());
            let available = HTTP1_CONNECTIONS.saturating_sub(reserved);
            if available < budget.lanes.len() {
                return Err(format!("selected servers share {origin} with insufficient HTTP/1 progress and control capacity").into());
            }
            let wanted: usize = budget.lanes.iter().map(|lane| lane.ceiling).sum();
            if wanted <= available {
                continue;
            }
            if config.streams > 0 {
                return Err(format!("forced streams at {origin} exceed shared HTTP/1 capacity; reduce streams or use Automatic").into());
            }
            for lane in &budget.lanes {
                *counts
                    .get_mut(lane.id)
                    .expect("registered participant")
                    .count(lane.direction) = 1;
            }
            let mut remaining = available - budget.lanes.len();
            while remaining > 0 {
                let before = remaining;
                for lane in &budget.lanes {
                    let count = counts
                        .get_mut(lane.id)
                        .expect("registered participant")
                        .count(lane.direction);
                    if remaining > 0 && *count < lane.ceiling {
                        *count += 1;
                        remaining -= 1;
                    }
                }
                if remaining == before {
                    break;
                }
            }
        }
        let download: usize = counts.values().map(|counts| counts.download).sum();
        let upload: usize = counts.values().map(|counts| counts.upload).sum();
        if download > MAX_DIRECTION_LANES || upload > MAX_DIRECTION_LANES {
            return Err("the run exceeds 128 streams per direction; reduce forced streams".into());
        }
        Ok(Self {
            participants: counts,
        })
    }

    pub fn lanes(&self, id: &str) -> Option<LaneCounts> {
        self.participants.get(id).copied()
    }
}

fn desired(config: &Config, target: &ThroughputTarget, direction: Direction) -> usize {
    if target.transport == ThroughputTransport::WebTransport {
        return if config.streams > 0 {
            config.streams.min(WT_LANES)
        } else {
            1
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn target(
        origin: &str,
        protocol: Protocol,
        transport: ThroughputTransport,
    ) -> ThroughputTarget {
        ThroughputTarget {
            base_url: origin.into(),
            protocol,
            transport,
        }
    }

    #[test]
    fn shared_origin_bidirectional_reserves_control_and_rejects_impossible_selection() {
        let config = Config::default();
        let first = target(
            "https://meter.example",
            Protocol::Http1,
            ThroughputTransport::FetchStream,
        );
        let alias = target(
            "https://METER.example:443",
            Protocol::Http1,
            ThroughputTransport::FetchStream,
        );
        let participants = [
            Participant {
                id: "a",
                throughput: Some(&first),
                latency: None,
            },
            Participant {
                id: "b",
                throughput: Some(&alias),
                latency: None,
            },
        ];
        // Four data lanes + two progress feeds + one finish connection exceed six.
        assert!(StageLanePlan::new(&config, Stage::Bidirectional, &participants).is_err());
        let plan = StageLanePlan::new(&config, Stage::Download, &participants).unwrap();
        assert_eq!(
            plan.lanes("a"),
            Some(LaneCounts {
                download: 3,
                upload: 0
            })
        );
        assert_eq!(plan.lanes("b"), plan.lanes("a"));
    }

    #[test]
    fn bidirectional_round_robin_keeps_progress_and_latency_capacity() {
        let config = Config::default();
        let throughput = target(
            "https://meter.example",
            Protocol::Http1,
            ThroughputTransport::FetchStream,
        );
        let latency = LatencyTarget {
            base_url: throughput.base_url.clone(),
            transport: LatencyTransport::WebSocket,
        };
        let participants = [Participant {
            id: "a",
            throughput: Some(&throughput),
            latency: Some(&latency),
        }];
        let plan = StageLanePlan::new(&config, Stage::Bidirectional, &participants).unwrap();
        assert_eq!(
            plan.lanes("a"),
            Some(LaneCounts {
                download: 2,
                upload: 1
            })
        );
        let forced = Config {
            streams: 2,
            ..config
        };
        assert!(StageLanePlan::new(&forced, Stage::Bidirectional, &participants).is_err());
    }

    #[test]
    fn forced_global_limit_applies_per_direction_after_webtransport_cap() {
        let h3 = target(
            "https://meter.example",
            Protocol::Http3,
            ThroughputTransport::FetchStream,
        );
        let wt = target(
            "https://meter.example",
            Protocol::Http3,
            ThroughputTransport::WebTransport,
        );
        let config = Config {
            streams: 128,
            ..Config::default()
        };
        let h3_participants = [
            Participant {
                id: "a",
                throughput: Some(&h3),
                latency: None,
            },
            Participant {
                id: "b",
                throughput: Some(&h3),
                latency: None,
            },
        ];
        assert!(StageLanePlan::new(&config, Stage::Download, &h3_participants).is_err());
        let at_limit = Config {
            streams: 64,
            ..config.clone()
        };
        let plan = StageLanePlan::new(&at_limit, Stage::Bidirectional, &h3_participants).unwrap();
        assert_eq!(
            plan.lanes("a"),
            Some(LaneCounts {
                download: 64,
                upload: 64
            })
        );
        assert_eq!(plan.lanes("b"), plan.lanes("a"));
        let wt_participants = [
            Participant {
                id: "a",
                throughput: Some(&wt),
                latency: None,
            },
            Participant {
                id: "b",
                throughput: Some(&wt),
                latency: None,
            },
        ];
        let plan = StageLanePlan::new(&config, Stage::Bidirectional, &wt_participants).unwrap();
        assert_eq!(
            plan.lanes("a"),
            Some(LaneCounts {
                download: 16,
                upload: 16
            })
        );
        assert_eq!(plan.lanes("b"), plan.lanes("a"));
        // An unloaded latency stage allocates no throughput lanes.
        assert_eq!(
            StageLanePlan::new(&config, Stage::Latency, &h3_participants)
                .unwrap()
                .lanes("a"),
            Some(LaneCounts::default())
        );
    }
}
