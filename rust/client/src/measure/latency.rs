//! One latency population's accounting, and the probe deadline (`docs/MEASUREMENTS.md`, `api/latency.testvectors.json`).

use std::{collections::BTreeMap, time::Duration};

/// A failed population shows its median only after this many replies and timeouts.
const FAILED_MEDIAN_OUTCOMES: usize = 3;
const DEADLINE_FLOOR: Duration = Duration::from_millis(250);
const DEADLINE_CEILING: Duration = Duration::from_secs(10);
const MIN_RTTVAR: Duration = Duration::from_millis(1);

/// What became of one probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// A matched reply after `rtt`, with the server's own handling time.
    Reply {
        rtt: Duration,
        handling: Duration,
    },
    /// The deadline passed first; a late reply does not change that.
    Timeout,
    /// The channel ended before a verdict.
    Unresolved,
    SendFailed,
}

/// The replies, timeouts and continuity of one server's probes in one stage.
#[derive(Debug, Clone, Default)]
pub struct Latency {
    /// Replies by round trip rounded to the microsecond.
    rtts: BTreeMap<Duration, usize>,
    /// The counts; `summary()` adds the statistics.
    counts: Summary,
    previous: Option<Duration>,
    variation: Duration,
    timed: usize,
    timed_rtt: Duration,
    handling: Duration,
}

/// A population's statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub replies: usize,
    pub timeouts: usize,
    pub unresolved: usize,
    pub send_failures: usize,
    pub p50: Option<Duration>,
    pub p95: Option<Duration>,
    pub jitter: Option<Duration>,
    pub jitter_pairs: usize,
    pub timing: Option<Timing>,
}

/// Mean raw round trip and server handling over the same replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub pairs: usize,
    pub rtt: Duration,
    pub handling: Duration,
}

impl Latency {
    pub fn record(&mut self, outcome: ProbeOutcome) {
        match outcome {
            ProbeOutcome::Reply { rtt, handling } => self.reply(rtt, handling),
            ProbeOutcome::Timeout => self.counts.timeouts += 1,
            ProbeOutcome::Unresolved => self.counts.unresolved += 1,
            ProbeOutcome::SendFailed => self.counts.send_failures += 1,
        }
    }

    /// A handling time above the round trip leaves the reply out of the paired timing only.
    fn reply(&mut self, rtt: Duration, handling: Duration) {
        if let Some(previous) = self.previous.replace(rtt) {
            self.variation = self.variation.saturating_add(rtt.abs_diff(previous));
            self.counts.jitter_pairs += 1;
        }
        let micros = Duration::from_micros(u64::try_from((rtt.as_nanos() + 500) / 1000).unwrap_or(u64::MAX));
        *self.rtts.entry(micros).or_default() += 1;
        self.counts.replies += 1;
        if handling <= rtt {
            self.timed += 1;
            self.timed_rtt = self.timed_rtt.saturating_add(rtt);
            self.handling = self.handling.saturating_add(handling);
        }
    }

    /// A reconnect or stage boundary: the next reply has no predecessor to vary from.
    pub fn break_continuity(&mut self) {
        self.previous = None;
    }

    pub fn summary(&self) -> Summary {
        let (replies, pairs) = (self.counts.replies, self.counts.jitter_pairs);
        let rank = |rank| self.nth(rank);
        let p50 = (replies > 0).then(|| {
            let lower = rank(replies.div_ceil(2));
            match replies % 2 {
                0 => lower + (rank(replies / 2 + 1) - lower) / 2,
                _ => lower,
            }
        });
        Summary {
            p50,
            p95: (replies > 0).then(|| rank((95 * replies).div_ceil(100))),
            jitter: (pairs > 0).then(|| mean(self.variation, pairs)),
            timing: (self.timed > 0).then(|| Timing {
                pairs: self.timed,
                rtt: mean(self.timed_rtt, self.timed),
                handling: mean(self.handling, self.timed),
            }),
            ..self.counts
        }
    }

    /// The reply of 1-based `rank` in round-trip order.
    fn nth(&self, mut rank: usize) -> Duration {
        for (&rtt, &count) in &self.rtts {
            if rank <= count {
                return rtt;
            }
            rank -= count;
        }
        unreachable!("ranks stay within the replies")
    }
}

impl Summary {
    /// Timeouts among resolved probes; none without a resolved probe.
    pub fn timeout_ratio(&self) -> Option<f64> {
        let resolved = self.replies + self.timeouts;
        (resolved > 0).then(|| self.timeouts as f64 / resolved as f64)
    }
}

fn mean(sum: Duration, count: usize) -> Duration {
    let nanos = sum.as_nanos() / count as u128;
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// A population as its stage leaves it; `complete` unless it failed or the stage stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Population {
    pub summary: Summary,
    pub complete: bool,
}

impl Population {
    /// The median a view shows: none without a reply, or for an incomplete population of too few outcomes.
    pub fn median(&self) -> Option<Duration> {
        let outcomes = self.summary.replies + self.summary.timeouts;
        match self.complete || outcomes >= FAILED_MEDIAN_OUTCOMES {
            true => self.summary.p50,
            false => None,
        }
    }
}

/// Loaded median minus idle median in milliseconds; negative values stay.
pub fn added(loaded: Option<Duration>, idle: Option<Duration>) -> Option<f64> {
    let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
    Some(ms(loaded?) - ms(idle?))
}

/// A probe's deadline, SRTT + 4 × max(RTTVAR, 1 ms) within 250 ms–10 s (RFC 6298), learnt from every reply.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deadline {
    /// SRTT and RTTVAR once a reply arrived.
    estimate: Option<(Duration, Duration)>,
}

impl Deadline {
    pub fn get(&self) -> Duration {
        self.estimate.map_or(DEADLINE_FLOOR, |(srtt, rttvar)| {
            srtt.saturating_add(rttvar.max(MIN_RTTVAR).saturating_mul(4))
                .clamp(DEADLINE_FLOOR, DEADLINE_CEILING)
        })
    }

    /// Learns from a reply, also one past its deadline.
    pub fn observe(&mut self, rtt: Duration) {
        self.estimate = Some(match self.estimate {
            None => (rtt, rtt / 2),
            Some((srtt, rttvar)) => (
                srtt.saturating_mul(7).saturating_add(rtt) / 8,
                rttvar.saturating_mul(3).saturating_add(srtt.abs_diff(rtt)) / 4,
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn handling_above_the_round_trip_omits_only_the_pair() {
        let mut latency = Latency::default();
        latency.record(ProbeOutcome::Reply { rtt: ms(10), handling: Duration::from_micros(100) });
        latency.record(ProbeOutcome::Reply { rtt: ms(20), handling: ms(21) });
        latency.record(ProbeOutcome::Reply { rtt: ms(30), handling: Duration::from_micros(300) });
        let summary = latency.summary();
        assert_eq!((summary.replies, summary.p50, summary.jitter), (3, Some(ms(20)), Some(ms(10))));
        let timing = Timing { pairs: 2, rtt: ms(20), handling: Duration::from_micros(200) };
        assert_eq!(summary.timing, Some(timing));
    }

    #[test]
    fn unfinished_probes_and_send_failures_stay_out_of_the_ratio() {
        let mut latency = Latency::default();
        for outcome in [ProbeOutcome::Unresolved, ProbeOutcome::SendFailed, ProbeOutcome::Timeout] {
            latency.record(outcome);
        }
        let summary = latency.summary();
        assert_eq!((summary.unresolved, summary.send_failures, summary.timeout_ratio()), (1, 1, Some(1.0)));
    }

    #[test]
    fn an_incomplete_population_shows_its_median_from_three_outcomes() {
        let mut latency = Latency::default();
        latency.record(ProbeOutcome::Reply { rtt: ms(10), handling: Duration::ZERO });
        latency.record(ProbeOutcome::Timeout);
        let population = |complete, latency: &Latency| Population { summary: latency.summary(), complete };
        assert_eq!(population(true, &latency).median(), Some(ms(10)));
        assert_eq!(population(false, &latency).median(), None);
        latency.record(ProbeOutcome::Timeout);
        assert_eq!(population(false, &latency).median(), Some(ms(10)));
        assert_eq!(population(true, &Latency::default()).median(), None);
    }
}
