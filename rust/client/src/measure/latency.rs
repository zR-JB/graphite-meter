//! Tracks a latency population and the probe deadline (`docs/MEASUREMENTS.md`, `api/latency.testvectors.json`).

use std::{collections::BTreeMap, time::Duration};

/// A failed population shows its median only after this many replies and timeouts.
const FAILED_MEDIAN_OUTCOMES: usize = 3;
const DEADLINE_FLOOR: Duration = Duration::from_millis(250);
const DEADLINE_CEILING: Duration = Duration::from_secs(10);
const MIN_RTTVAR: Duration = Duration::from_millis(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// A reply matched after `rtt` and carried the server's own handling time.
    Reply {
        rtt: Duration,
        handling: Duration,
    },
    /// The deadline passed first; a late reply does not change that.
    Timeout,
    Unresolved,
    SendFailed,
}

#[derive(Debug, Clone, Default)]
pub struct Latency {
    /// Counts replies by round trip rounded to the microsecond.
    rtts: BTreeMap<Duration, usize>,
    counts: Summary,
    previous: Option<Duration>,
    variation: Duration,
    timed: usize,
    timed_rtt: Duration,
    handling: Duration,
}

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
    pub spread: Option<Spread>,
}

/// A box plot draws the fastest reply, the 10th and 90th percentiles and the slowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spread {
    pub min: Duration,
    pub p10: Duration,
    pub p90: Duration,
    pub max: Duration,
}

/// Averages the raw round trip and the server handling over the same replies.
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
        let micros =
            Duration::from_micros(u64::try_from((rtt.as_nanos() + 500) / 1000).unwrap_or(u64::MAX));
        *self.rtts.entry(micros).or_default() += 1;
        self.counts.replies += 1;
        if handling <= rtt {
            self.timed += 1;
            self.timed_rtt = self.timed_rtt.saturating_add(rtt);
            self.handling = self.handling.saturating_add(handling);
        }
    }

    /// After a reconnect or stage boundary the next reply has no predecessor to vary from.
    pub fn break_continuity(&mut self) {
        self.previous = None;
    }

    pub fn summary(&self) -> Summary {
        let (replies, pairs) = (self.counts.replies, self.counts.jitter_pairs);
        let percentile = |share: usize| (share * replies).div_ceil(100).max(1);
        // An even count takes its two middle replies, an odd one its single middle reply twice.
        let middle = (replies.div_ceil(2), replies / 2 + 1);
        let ranks = [
            1,
            percentile(10),
            middle.0,
            middle.1,
            percentile(90),
            percentile(95),
            replies,
        ];
        let ranked = (replies > 0).then(|| self.ranked(ranks));
        Summary {
            p50: ranked.map(|[_, _, lower, upper, ..]| lower + (upper - lower) / 2),
            p95: ranked.map(|[.., p95, _]| p95),
            jitter: (pairs > 0).then(|| mean(self.variation, pairs)),
            timing: (self.timed > 0).then(|| Timing {
                pairs: self.timed,
                rtt: mean(self.timed_rtt, self.timed),
                handling: mean(self.handling, self.timed),
            }),
            spread: ranked.map(|[min, p10, _, _, p90, _, max]| Spread { min, p10, p90, max }),
            ..self.counts
        }
    }

    /// Finds the replies at the ascending 1-based `ranks` of round-trip order in one walk over the histogram.
    fn ranked<const N: usize>(&self, ranks: [usize; N]) -> [Duration; N] {
        debug_assert!(ranks.is_sorted(), "{ranks:?}");
        let (mut found, mut next, mut seen) = ([Duration::ZERO; N], 0, 0);
        for (&rtt, &count) in &self.rtts {
            seen += count;
            while next < N && ranks[next] <= seen {
                found[next] = rtt;
                next += 1;
            }
            if next == N {
                break;
            }
        }
        found
    }
}

impl Summary {
    pub fn timeout_ratio(&self) -> Option<f64> {
        let resolved = self.replies + self.timeouts;
        (resolved > 0).then(|| self.timeouts as f64 / resolved as f64)
    }
}

fn mean(sum: Duration, count: usize) -> Duration {
    let nanos = sum.as_nanos() / count as u128;
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Captures a population as its stage leaves it. It is `complete` unless it failed or the stage stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Population {
    pub summary: Summary,
    pub complete: bool,
}

impl Population {
    /// Returns the median a view shows, none without a reply or for an incomplete population of too few outcomes.
    pub fn median(&self) -> Option<Duration> {
        let counted =
            self.complete || self.summary.replies + self.summary.timeouts >= FAILED_MEDIAN_OUTCOMES;
        self.summary.p50.filter(|_| counted)
    }
}

/// Subtracts the idle median from the loaded one in milliseconds. Negative values stay.
pub fn added(loaded: Option<Duration>, idle: Option<Duration>) -> Option<f64> {
    let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
    Some(ms(loaded?) - ms(idle?))
}

/// A probe's deadline is SRTT + 4 × max(RTTVAR, 1 ms) within 250 ms to 10 s (RFC 6298), learnt from every reply.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deadline {
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
    fn one_walk_finds_every_rank_a_sorted_list_does() {
        for count in 1..=40_u64 {
            let mut latency = Latency::default();
            // The round trips repeat in scrambled order.
            let mut rtts: Vec<_> = (0..count)
                .map(|index| ms(1 + (index * 7919) % 13))
                .collect();
            for &rtt in &rtts {
                latency.record(ProbeOutcome::Reply {
                    rtt,
                    handling: Duration::ZERO,
                });
            }
            rtts.sort();
            let n = rtts.len();
            let rank = |rank: usize| rtts[rank - 1];
            let percentile = |share: usize| rank((share * n).div_ceil(100).max(1));
            let lower = rank(n.div_ceil(2));
            let p50 = if n % 2 == 0 {
                lower + (rank(n / 2 + 1) - lower) / 2
            } else {
                lower
            };
            let spread = Spread {
                min: rtts[0],
                p10: percentile(10),
                p90: percentile(90),
                max: rtts[n - 1],
            };
            let summary = latency.summary();
            assert_eq!(
                (summary.p50, summary.p95),
                (Some(p50), Some(percentile(95))),
                "{count} replies"
            );
            assert_eq!(summary.spread, Some(spread), "{count} replies");
        }
    }

    #[test]
    fn an_incomplete_population_shows_its_median_from_three_outcomes() {
        let mut latency = Latency::default();
        latency.record(ProbeOutcome::Reply {
            rtt: ms(10),
            handling: Duration::ZERO,
        });
        latency.record(ProbeOutcome::Timeout);
        let population = |complete, latency: &Latency| Population {
            summary: latency.summary(),
            complete,
        };
        assert_eq!(population(true, &latency).median(), Some(ms(10)));
        assert_eq!(population(false, &latency).median(), None);
        latency.record(ProbeOutcome::Timeout);
        assert_eq!(population(false, &latency).median(), Some(ms(10)));
        assert_eq!(population(true, &Latency::default()).median(), None);
    }
}
