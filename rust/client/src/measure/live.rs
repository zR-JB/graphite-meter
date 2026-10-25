//! Mirrors the browser's live rate (`client/src/engine/measure/live.ts`, `api/liverate.testvectors.json`).

const MIN_WINDOW_MS: f64 = 800.0;
const FIRST_MS: f64 = 500.0;
const FAST_WINDOW_MS: f64 = 750.0;
const REGIME_READY_MS: f64 = 2000.0;
const DOWNSHIFT_MS: f64 = 750.0;
const UPSHIFT_MS: f64 = 500.0;
/// Spans kept past this many merge in pairs from the oldest half, so a long stage holds a bounded history.
const MAX_SPANS: usize = 4096;

#[derive(Debug, Clone, Copy, Default)]
struct Span {
    start: f64,
    end: f64,
    bytes: f64,
    total: f64,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    up: bool,
    start: f64,
    reference: f64,
    before: f64,
}

/// A confirmed regime change: how much evidence ago the live rate first read it, and the rate it read before.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shift {
    pub since_ms: f64,
    pub from: f64,
}

/// Derives a stage's presented rate from bytes over time. The first delivery only anchors it.
#[derive(Debug, Clone, Default)]
pub struct LiveRate {
    anchored: bool,
    spans: Vec<Span>,
    head: usize,
    bytes: f64,
    evidence: f64,
    regime_start: f64,
    fast: f64,
    candidate: Option<Candidate>,
    presented: f64,
}

/// The presentation window spans 85% of the current regime, never less than 800 ms.
fn window(age: f64) -> f64 {
    let age = if age.is_nan() { 0.0 } else { age.max(0.0) };
    age.min(MIN_WINDOW_MS.max(age * 0.85))
}

impl LiveRate {
    pub fn presented(&self) -> Option<f64> {
        Some(self.presented).filter(|&rate| rate > 0.0)
    }

    pub fn observe(&mut self, bytes: f64, ms: f64) -> Option<Shift> {
        self.step(bytes, ms, MAX_SPANS)
    }

    fn step(&mut self, bytes: f64, ms: f64, kept: usize) -> Option<Shift> {
        if !ms.is_finite() || ms <= 0.0 {
            return None;
        }
        if !self.anchored {
            self.anchored = bytes > 0.0;
            return None;
        }
        let start = self.evidence;
        self.evidence += ms;
        let bytes = if bytes > 0.0 { bytes } else { 0.0 };
        self.bytes += bytes;
        self.spans.push(Span {
            start,
            end: self.evidence,
            bytes,
            total: self.bytes,
        });
        self.recalculate();
        let shift = self.regime(start);
        if shift.is_some() {
            self.recalculate();
        }
        let mut keep = (self.evidence - window(self.evidence - self.regime_start))
            .min(self.regime_start.max(self.evidence - FAST_WINDOW_MS));
        if let Some(candidate) = self.candidate {
            keep = keep.min(candidate.start);
        }
        while self.head < self.spans.len() - 1 && self.spans[self.head].end <= keep {
            self.head += 1;
        }
        if self.head >= 1024 && self.head * 2 >= self.spans.len() {
            self.spans.drain(..self.head);
            self.head = 0;
        }
        if self.spans.len() - self.head > kept {
            self.coarsen();
        }
        shift
    }

    /// Merges the older spans in pairs, all but the fast window and a pending regime change, as the browser does.
    fn coarsen(&mut self) {
        let candidate = self
            .candidate
            .map_or(f64::INFINITY, |candidate| candidate.start);
        let fine = (self.evidence - 2.0 * FAST_WINDOW_MS).min(candidate);
        let older = self.spans[self.head..]
            .iter()
            .skip(1)
            .step_by(2)
            .take_while(|span| span.end <= fine)
            .count()
            * 2;
        let merged: Vec<_> = self.spans[self.head..self.head + older]
            .chunks(2)
            .map(|pair| Span {
                end: pair[1].end,
                bytes: pair[0].bytes + pair[1].bytes,
                total: pair[1].total,
                ..pair[0]
            })
            .collect();
        self.spans.splice(..self.head + older, merged);
        self.head = 0;
    }

    fn recalculate(&mut self) {
        self.presented = match self.evidence < FIRST_MS {
            true => 0.0,
            false => self.rate_since(self.evidence - window(self.evidence - self.regime_start)),
        };
        self.fast = self.rate_since(self.regime_start.max(self.evidence - FAST_WINDOW_MS));
    }

    /// Prorates the span the window starts in, so a window edge never rescans a long stage.
    fn rate_since(&self, start: f64) -> f64 {
        let spans = &self.spans[self.head..];
        let first = spans[spans
            .partition_point(|span| span.end <= start)
            .min(spans.len() - 1)];
        let from = start.max(first.start);
        let ms = self.evidence - from;
        if ms.is_nan() || ms <= 0.0 {
            return 0.0;
        }
        let partial = first.bytes * ((first.end - from) / (first.end - first.start));
        (self.bytes - first.total + partial) * 1000.0 / ms
    }

    fn regime(&mut self, start: f64) -> Option<Shift> {
        let Some(candidate) = self.candidate else {
            if self.evidence - self.regime_start < REGIME_READY_MS || self.presented <= 0.0 {
                return None;
            }
            let ratio = self.fast / self.presented;
            if !(0.75..=1.2).contains(&ratio) {
                // What the window read before the fast window that holds the change.
                let window = window(self.evidence - self.regime_start);
                let before = (self.presented * window - self.fast * FAST_WINDOW_MS)
                    / (window - FAST_WINDOW_MS);
                self.candidate = Some(Candidate {
                    up: ratio > 1.2,
                    start,
                    reference: self.presented,
                    before,
                });
            }
            return None;
        };
        let ratio = if candidate.reference > 0.0 {
            self.fast / candidate.reference
        } else {
            1.0
        };
        if candidate.up && ratio < 1.1 || !candidate.up && ratio > 0.85 {
            self.candidate = None;
            return None;
        }
        let confirm = if candidate.up {
            UPSHIFT_MS
        } else {
            DOWNSHIFT_MS
        };
        if self.evidence - candidate.start < confirm {
            return None;
        }
        (self.regime_start, self.candidate) = (candidate.start, None);
        Some(Shift {
            since_ms: self.evidence - candidate.start,
            from: candidate.before,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples the rate a stepped transfer presents every 250 ms, coarsening spans past `limit`.
    fn stepped(limit: usize) -> (Vec<f64>, usize) {
        let mut rate = LiveRate::default();
        let presented = (0..40_000)
            .map(|step| {
                let bytes = [1000.0, 4000.0, 2500.0][step / 9000 % 3] + (step % 7) as f64;
                rate.step(bytes, 250.0, limit);
                rate.presented().unwrap_or_default()
            })
            .collect();
        (presented, rate.spans.len() - rate.head)
    }

    #[test]
    fn a_long_stage_keeps_a_bounded_history_and_presents_what_the_whole_history_would() {
        let ((bounded, kept), (whole, _)) = (stepped(MAX_SPANS), stepped(usize::MAX));
        assert!(kept <= MAX_SPANS, "{kept}");
        for (step, (bounded, whole)) in bounded.iter().zip(&whole).enumerate() {
            assert!(
                (bounded - whole).abs() <= 1e-3 * whole.max(1.0),
                "step {step}: {bounded} != {whole}"
            );
        }
    }
}
