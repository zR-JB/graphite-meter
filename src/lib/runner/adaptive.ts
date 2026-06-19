/* ============================================================
 * The Graphite Meter — Adaptive Duration helper (§13.4)
 * Runner-agnostic confidence math for confidence-based early
 * phase exit. Pure TypeScript, zero Svelte / DOM deps so a real
 * engine can reuse it verbatim. Ported (de-magicked) from
 * linerate's DummyRunner.transferConfidence / latencyConfidence /
 * shouldAdvanceEarly logic; every coefficient below is named and
 * commented per §13.0 ("de-magic on the way in").
 * ============================================================ */

import type { AdaptiveDurationConfig, StabilityBand } from "./contract";

/* ---------- Stability-score coefficients ----------
 * The stability score is  1 − (varianceRatio·K1 + slopeRatio·K2),
 * clamped to 0–1. Higher coefficients make the gate stricter
 * (a small amount of jitter/drift pulls the score down faster).
 * Lifted from linerate (variance ×2.2, slope ×1.4 for transfer;
 * variance ×2.4, loss ×3.6 for latency) and named here. */

/** Transfer stability: how hard sample-to-mean variance is penalized. */
const TRANSFER_VARIANCE_K = 2.2;
/** Transfer stability: how hard a first-vs-last segment drift is penalized. */
const TRANSFER_SLOPE_K = 1.4;

/** Latency stability: how hard RTT variance is penalized. */
const LATENCY_VARIANCE_K = 2.4;
/** Latency stability: how hard packet loss within the window is penalized. */
const LATENCY_LOSS_K = 3.6;

/** Only the most recent N samples feed the confidence window — older
 *  samples from a phase's ramp-up would otherwise depress stability. */
const CONFIDENCE_WINDOW = 48;

/** Slope is measured as |mean(firstSegment) − mean(lastSegment)| / mean.
 *  The window is split into this many segments (first vs last third). */
const SLOPE_SEGMENTS = 3;
/** Minimum samples per slope segment so a 2-sample window still works. */
const MIN_SLOPE_SEGMENT = 2;

/** Score at/above this (but below stabilityThreshold) reads as the "medium"
 *  pip band; below it reads "low". The "high" band starts at the early-exit
 *  gate (stabilityThreshold), so "green pip" and "ready to finish early"
 *  coincide — one signal, no second meaning to reconcile. */
export const STABILITY_MED_BAND = 0.6;

/** Map a 0–1 stability score to its coarse pip band (stateless). */
export function stabilityBand(
  score: number,
  cfg: AdaptiveDurationConfig,
): StabilityBand {
  if (score >= cfg.stabilityThreshold) return "high";
  if (score >= STABILITY_MED_BAND) return "medium";
  return "low";
}

/** Hysteresis margin below `stabilityThreshold` for *leaving* the stable state.
 *  A connection becomes "stable" only at the full threshold, but stays stable
 *  until it drops this far below it — so the pip and the stable window don't
 *  flicker on and off around the boundary. Confidence shouldn't toggle. */
export const STABILITY_HYSTERESIS = 0.08;

/** Schmitt trigger for the "stable" state: enter at `stabilityThreshold`, leave
 *  only once the score falls below `stabilityThreshold − STABILITY_HYSTERESIS`.
 *  In the dead band between the two it holds whatever it already was. */
export function isStillStable(
  wasStable: boolean,
  score: number,
  cfg: AdaptiveDurationConfig,
): boolean {
  const enter = cfg.stabilityThreshold;
  const exit = enter - STABILITY_HYSTERESIS;
  return wasStable ? score >= exit : score >= enter;
}

/** Band from the *latched* stable state (hysteretic): a sustained stable run
 *  reads "high"; otherwise the score's climb shows as medium/low. */
export function bandForState(stable: boolean, score: number): StabilityBand {
  if (stable) return "high";
  if (score >= STABILITY_MED_BAND) return "medium";
  return "low";
}

/* ---------- Pure stats (mirrors measurement-style helpers) ---------- */

export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

export function mean(values: number[]): number {
  if (!values.length) return 0;
  return values.reduce((sum, v) => sum + v, 0) / values.length;
}

export function standardDeviation(values: number[]): number {
  if (values.length < 2) return 0;
  const avg = mean(values);
  const variance = values.reduce((s, v) => s + (v - avg) ** 2, 0) / values.length;
  return Math.sqrt(variance);
}

/* ---------- Confidence shapes ---------- */

export interface ConfidenceScore {
  /** 0–1 stability score: 1 = rock-steady, 0 = noisy/drifting. */
  score: number;
  /** stddev / mean of the windowed values (coefficient of variation). */
  varianceRatio: number;
  /** |first-third mean − last-third mean| / mean — a drift indicator. */
  slopeRatio: number;
  /** how many usable samples were in the window. */
  sampleCount: number;
}

export interface LatencyConfidenceScore extends Omit<ConfidenceScore, "slopeRatio"> {
  /** fraction of the windowed pings that were lost. */
  lossRatio: number;
}

/**
 * Transfer (download/upload) confidence from a window of bytes/sec values.
 * Stability falls as the plateau gets noisier (variance) or keeps drifting
 * up/down (slope). Returns score 0 when there is not enough signal yet.
 */
export function transferConfidence(bytesPerSecValues: number[]): ConfidenceScore {
  const values = bytesPerSecValues.slice(-CONFIDENCE_WINDOW);
  if (values.length < 2) {
    return { score: 0, varianceRatio: 1, slopeRatio: 1, sampleCount: values.length };
  }

  const avg = mean(values);
  const varianceRatio = avg > 0 ? standardDeviation(values) / avg : 1;

  const segmentSize = Math.max(MIN_SLOPE_SEGMENT, Math.ceil(values.length / SLOPE_SEGMENTS));
  const first = mean(values.slice(0, segmentSize));
  const last = mean(values.slice(-segmentSize));
  const slopeRatio = avg > 0 ? Math.abs(last - first) / avg : 1;

  const score = clamp(
    1 - varianceRatio * TRANSFER_VARIANCE_K - slopeRatio * TRANSFER_SLOPE_K,
    0,
    1,
  );

  return { score, varianceRatio, slopeRatio, sampleCount: values.length };
}

/**
 * Latency confidence from a window of unloaded RTT values plus the loss count
 * over the same window. Stability falls with jitter (variance) and any loss.
 */
export function latencyConfidence(
  rttValues: number[],
  windowSampleCount: number,
  lostInWindow: number,
): LatencyConfidenceScore {
  const values = rttValues.slice(-CONFIDENCE_WINDOW);
  if (values.length < 2) {
    return { score: 0, varianceRatio: 1, lossRatio: 1, sampleCount: values.length };
  }

  const avg = mean(values);
  const varianceRatio = avg > 0 ? standardDeviation(values) / avg : 1;
  const lossRatio = windowSampleCount > 0 ? lostInWindow / windowSampleCount : 0;

  const score = clamp(
    1 - varianceRatio * LATENCY_VARIANCE_K - lossRatio * LATENCY_LOSS_K,
    0,
    1,
  );

  return { score, varianceRatio, lossRatio, sampleCount: values.length };
}

/* ---------- Early-exit predicate ---------- */

export interface ExitDecisionInput {
  /** which kind of phase is being evaluated. */
  kind: "latency" | "transfer";
  /** ms elapsed within the current phase. */
  elapsedMs: number;
  /** nominal (configured) duration of the phase, ms. */
  durationMs: number;
  /** the relevant confidence score for this phase. */
  confidence: ConfidenceScore | LatencyConfidenceScore;
  /** adaptive config (coverage / stability / min-sample floors). */
  cfg: AdaptiveDurationConfig;
}

/**
 * True once it is safe to end the current phase early. A phase may only exit
 * when ALL hold:
 *   1. coverage ≥ minCoverageRatio  — enough of the nominal duration is done;
 *      and never less than (1 − maxPhaseReductionRatio), so we never cut a
 *      phase by more than the configured max (the rail/progress stays honest);
 *   2. stability score ≥ stabilityThreshold;
 *   3. the relevant min-sample floor is met.
 * Returns false whenever adaptive is disabled or the phase is degenerate.
 */
export function shouldExitPhase(input: ExitDecisionInput): boolean {
  const { kind, elapsedMs, durationMs, confidence, cfg } = input;
  if (!cfg.enabled) return false;
  if (durationMs <= 0) return false;

  // Coverage floor: honour BOTH minCoverageRatio and the max-reduction cap,
  // so a phase is never shortened past (1 − maxPhaseReductionRatio).
  const requiredCoverage = Math.max(cfg.minCoverageRatio, 1 - cfg.maxPhaseReductionRatio);
  if (elapsedMs / durationMs < requiredCoverage) return false;

  if (confidence.score < cfg.stabilityThreshold) return false;

  const floor = kind === "latency" ? cfg.minLatencySamples : cfg.minTransferSamples;
  return confidence.sampleCount >= floor;
}
