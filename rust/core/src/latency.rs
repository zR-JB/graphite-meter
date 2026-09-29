//! Pure probe accounting. All times are integer nanoseconds on the client's monotonic clock.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    Reply { rtt_nanos: i64, handling_nanos: u64 },
    Timeout,
    Unresolved,
    SendFailure,
}

/// The percentiles the reports show, as Go's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Distribution {
    pub p50: u64,
    pub p95: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReflectorTiming {
    pub count: usize,
    pub mean_raw_rtt: u64,
    pub mean_handling: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencySummary {
    pub distribution: Option<Distribution>,
    pub jitter: Option<u64>,
    pub jitter_pairs: usize,
    pub reflector_timing: Option<ReflectorTiming>,
    pub count: usize,
    pub timeouts: usize,
    pub unresolved: usize,
    pub send_failures: usize,
}

impl LatencySummary {
    pub fn timeout_ratio(self) -> Option<f64> {
        let resolved = self.count + self.timeouts;
        (resolved > 0).then(|| self.timeouts as f64 / resolved as f64)
    }
}

/// Probe deadline SRTT + 4·max(RTTVAR, 1 ms) within 250 ms–10 s, learning from every
/// matched reply, including ones that arrive after their deadline.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeadlineEstimator {
    srtt_rttvar: Option<(u64, u64)>,
}

impl DeadlineEstimator {
    pub const FLOOR_NANOS: u64 = 250_000_000;
    pub const CEIL_NANOS: u64 = 10_000_000_000;

    pub fn deadline_nanos(self) -> u64 {
        self.srtt_rttvar.map_or(Self::FLOOR_NANOS, |(srtt, rttvar)| {
            srtt.saturating_add(rttvar.max(1_000_000).saturating_mul(4))
                .clamp(Self::FLOOR_NANOS, Self::CEIL_NANOS)
        })
    }

    pub fn observe(&mut self, rtt: u64) {
        self.srtt_rttvar = Some(match self.srtt_rttvar {
            None => (rtt, rtt / 2),
            Some((srtt, rttvar)) => (
                srtt.saturating_mul(7).saturating_add(rtt) / 8,
                rttvar.saturating_mul(3).saturating_add(srtt.abs_diff(rtt)) / 4,
            ),
        });
    }
}

#[derive(Debug, Default)]
pub struct LatencyAccumulator {
    rtts: Vec<u64>,
    previous: Option<u64>,
    variation_sum: u128,
    jitter_pairs: usize,
    timeouts: usize,
    unresolved: usize,
    send_failures: usize,
    timing_count: usize,
    timing_raw_sum: u128,
    handling_sum: u128,
}

impl LatencyAccumulator {
    /// Returns the validated server handling interval for an accepted reply.
    /// Missing or impossible diagnostics never discard a valid raw RTT.
    pub fn record(&mut self, outcome: ProbeOutcome) -> Option<u64> {
        match outcome {
            ProbeOutcome::Timeout => self.timeouts += 1,
            ProbeOutcome::Unresolved => self.unresolved += 1,
            ProbeOutcome::SendFailure => self.send_failures += 1,
            ProbeOutcome::Reply {
                rtt_nanos,
                handling_nanos,
            } => {
                let Ok(rtt) = u64::try_from(rtt_nanos) else {
                    return None;
                };
                if let Some(previous) = self.previous {
                    self.variation_sum += u128::from(rtt.abs_diff(previous));
                    self.jitter_pairs += 1;
                }
                self.previous = Some(rtt);
                self.rtts.push(rtt);
                if handling_nanos <= i64::MAX as u64 && handling_nanos <= rtt {
                    self.timing_count += 1;
                    self.timing_raw_sum += u128::from(rtt);
                    self.handling_sum += u128::from(handling_nanos);
                    return Some(handling_nanos);
                }
            }
        }
        None
    }

    /// Call when a connection closes or pending probes are settled at a stage boundary.
    pub fn break_continuity(&mut self) {
        self.previous = None;
    }

    pub fn snapshot(&self) -> LatencySummary {
        let mut out = LatencySummary {
            count: self.rtts.len(),
            timeouts: self.timeouts,
            unresolved: self.unresolved,
            send_failures: self.send_failures,
            jitter_pairs: self.jitter_pairs,
            ..LatencySummary::default()
        };
        if self.jitter_pairs > 0 {
            out.jitter = Some((self.variation_sum / self.jitter_pairs as u128) as u64);
        }
        if self.timing_count > 0 {
            let count = self.timing_count as u128;
            out.reflector_timing = Some(ReflectorTiming {
                count: self.timing_count,
                mean_raw_rtt: (self.timing_raw_sum / count) as u64,
                mean_handling: (self.handling_sum / count) as u64,
            });
        }
        if !self.rtts.is_empty() {
            let mut sorted = self.rtts.clone();
            sorted.sort_unstable();
            let middle = sorted.len() / 2;
            let p50 = if sorted.len().is_multiple_of(2) {
                sorted[middle - 1] + (sorted[middle] - sorted[middle - 1]) / 2
            } else {
                sorted[middle]
            };
            out.distribution = Some(Distribution {
                p50,
                p95: nearest_rank(&sorted, 95),
            });
        }
        out
    }
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    // ceil(percentile * n / 100), written without floating-point rounding.
    let rank = (percentile * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}
