//! The live rate the browser presents (`client/src/lib/runner/liveRates.ts`, `api/liverate.testvectors.json`): a
//! growing average that restarts on a confirmed shift.

const MIN_WINDOW_MS: f64 = 800.0;
const FIRST_MS: f64 = 500.0;
const FAST_WINDOW_MS: f64 = 750.0;
const REGIME_READY_MS: f64 = 2000.0;
const DOWNSHIFT_MS: f64 = 750.0;
const UPSHIFT_MS: f64 = 500.0;

/// Bytes over a stretch of evidence, and all bytes up to its end.
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
}

/// A stage's presented rate from bytes over time; the first delivery only anchors it.
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

/// The presentation window: 85% of the current regime, never less than 800 ms.
fn window(age: f64) -> f64 {
    let age = if age.is_nan() { 0.0 } else { age.max(0.0) };
    age.min(MIN_WINDOW_MS.max(age * 0.85))
}

impl LiveRate {
    /// The rate shown in bytes per second; zero while there is none.
    pub fn presented(&self) -> f64 {
        self.presented
    }

    /// Adds `bytes` measured over `ms` and reports whether a new regime was confirmed.
    pub fn observe(&mut self, bytes: f64, ms: f64) -> bool {
        if !ms.is_finite() || ms <= 0.0 {
            return false;
        }
        if !self.anchored {
            self.anchored = bytes > 0.0;
            return false;
        }
        let start = self.evidence;
        self.evidence += ms;
        let bytes = if bytes > 0.0 { bytes } else { 0.0 };
        self.bytes += bytes;
        self.spans
            .push(Span { start, end: self.evidence, bytes, total: self.bytes });
        self.recalculate();
        let changed = self.regime(start);
        if changed {
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
        changed
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
        let first = spans[spans.partition_point(|span| span.end <= start).min(spans.len() - 1)];
        let from = start.max(first.start);
        let ms = self.evidence - from;
        if ms.is_nan() || ms <= 0.0 {
            return 0.0;
        }
        let partial = first.bytes * ((first.end - from) / (first.end - first.start));
        (self.bytes - first.total + partial) * 1000.0 / ms
    }

    fn regime(&mut self, start: f64) -> bool {
        let Some(candidate) = self.candidate else {
            if self.evidence - self.regime_start < REGIME_READY_MS || self.presented <= 0.0 {
                return false;
            }
            let ratio = self.fast / self.presented;
            if !(0.75..=1.2).contains(&ratio) {
                self.candidate = Some(Candidate { up: ratio > 1.2, start, reference: self.presented });
            }
            return false;
        };
        let ratio = if candidate.reference > 0.0 { self.fast / candidate.reference } else { 1.0 };
        if candidate.up && ratio < 1.1 || !candidate.up && ratio > 0.85 {
            self.candidate = None;
            return false;
        }
        let confirm = if candidate.up { UPSHIFT_MS } else { DOWNSHIFT_MS };
        if self.evidence - candidate.start < confirm {
            return false;
        }
        (self.regime_start, self.candidate) = (candidate.start, None);
        true
    }
}
