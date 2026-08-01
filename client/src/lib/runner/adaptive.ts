/* ============================================================
 * The Graphite Meter: adaptive duration helper
 * Runner-agnostic confidence math for confidence-based early
 * phase exit. Pure TypeScript, zero Svelte or DOM deps, so any
 * engine reuses it verbatim.
 * ============================================================ */

import type {
  AdaptiveDurationConfig,
  PingCadence,
  StabilityBand,
} from "./contract";
import { TRANSFER_CONTROL_BUCKET_MS } from "./controlBuckets";
import { fixedPingIntervalMs } from "./pingCadence";
import { median } from "./stats";

/* ---------- Stability-score coefficients ----------
 * score = clamp(1 − (varianceRatio·K1 + slopeRatio·K2), 0, 1).
 * Higher coefficients make the gate stricter: a small amount of
 * jitter or drift pulls the score down faster. */

/** Transfer stability: penalty on sample-to-mean variance (coefficient of
 *  variation), which stays small on a settled plateau. */
const TRANSFER_VARIANCE_K = 2.2;
/** Transfer stability: penalty on first-vs-last segment drift. Sustained drift
 *  means the transfer has not settled, so it withholds an early exit. */
const TRANSFER_SLOPE_K = 1.4;

/** Latency stability: how hard sustained RTT jitter is penalized. */
const LATENCY_JITTER_K = 1.2;
/** Latency stability: how hard packet loss within the window is penalized. */
const LATENCY_LOSS_K = 3.6;
/** Below this baseline, small timer/network noise is treated in absolute ms
 * rather than magnified by division through a tiny loopback/LAN RTT. */
const LATENCY_JITTER_FLOOR_MS = 20;

/** Fixed transfer-control horizon, expressed once in time and resolved through
 * the canonical bucket duration so tuning either cannot silently drift. */
export const TRANSFER_CONFIDENCE_WINDOW_MS = 4_000;
export const TRANSFER_CONFIDENCE_BUCKETS = Math.floor(
  TRANSFER_CONFIDENCE_WINDOW_MS / TRANSFER_CONTROL_BUCKET_MS,
);
/** Latency confidence is selected by event time, never callback count. */
export const LATENCY_CONFIDENCE_WINDOW_MS = 4_000;

/** Statistical floors retained when a configured sample target must be capped
 * to fit a phase. Explicitly lower advanced config remains authoritative. */
const MIN_LATENCY_CONFIDENCE_SAMPLES = 3;
const MIN_TRANSFER_CONFIDENCE_SAMPLES = 4;

/** Slope is measured as |mean(firstSegment) − mean(lastSegment)| / mean.
 *  The window is split into this many segments (first vs last third). */
const SLOPE_SEGMENTS = 3;
/** Minimum samples per slope segment so a 2-sample window still works. */
const MIN_SLOPE_SEGMENT = 2;

/** Score at or above this, and below stabilityThreshold, reads as the "medium"
 *  pip band; below it reads "low". "high" starts at stabilityThreshold, so the
 *  green pip and "ready to finish early" are one signal. */
const STABILITY_MED_BAND = 0.6;

/** Hysteresis margin below `stabilityThreshold` for leaving the stable state.
 *  Entry needs the full threshold; exit needs a drop this far below it, so the
 *  pip and the stable window do not flicker at the boundary. */
const STABILITY_HYSTERESIS = 0.08;

/** Schmitt trigger for the "stable" state: enter at `stabilityThreshold`, leave
 *  below `stabilityThreshold − STABILITY_HYSTERESIS`. In the dead band it keeps
 *  the state it carries in. */
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

/* ---------- Pure stats ---------- */

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function mean(values: number[]): number {
  if (!values.length) return 0;
  return values.reduce((sum, v) => sum + v, 0) / values.length;
}

export function standardDeviation(values: number[]): number {
  if (values.length < 2) return 0;
  const avg = mean(values);
  const variance =
    values.reduce((s, v) => s + (v - avg) ** 2, 0) / values.length;
  return Math.sqrt(variance);
}

/* ---------- Confidence shapes ---------- */

export interface ConfidenceScore {
  /** Stability score in 0..1: 1 = rock-steady, 0 = noisy/drifting. */
  score: number;
  /** stddev / mean of the windowed values (coefficient of variation). */
  varianceRatio: number;
  /** |first-third mean − last-third mean| / mean, a drift indicator. */
  slopeRatio: number;
  /** usable samples in the window. */
  sampleCount: number;
}

export interface LatencyConfidenceScore extends Omit<
  ConfidenceScore,
  "slopeRatio" | "varianceRatio"
> {
  /** median absolute RTT deviation / max(median RTT, jitter floor). */
  jitterRatio: number;
  /** fraction of the windowed pings lost. */
  lossRatio: number;
}

export interface TimedLatencyOutcome {
  tMs: number;
  rttMs: number | null;
}

/**
 * Transfer (download/upload) confidence from a window of bytes/sec values.
 * Stability falls as the plateau gets noisier (variance) or keeps drifting
 * up/down (slope). Returns score 0 when there is not enough signal yet.
 */
export function transferConfidence(
  bytesPerSecValues: number[],
): ConfidenceScore {
  const values = bytesPerSecValues.slice(-TRANSFER_CONFIDENCE_BUCKETS);
  if (values.length < 2) {
    return {
      score: 0,
      varianceRatio: 1,
      slopeRatio: 1,
      sampleCount: values.length,
    };
  }

  const avg = mean(values);
  const varianceRatio = avg > 0 ? standardDeviation(values) / avg : 1;

  const segmentSize = Math.max(
    MIN_SLOPE_SEGMENT,
    Math.ceil(values.length / SLOPE_SEGMENTS),
  );
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
 * Latency confidence from unloaded ping outcomes. RTT and loss use the same
 * trailing window; robust median deviation ignores isolated tail spikes while
 * sustained jitter and packet loss still lower confidence.
 */
export function latencyConfidence(
  outcomes: TimedLatencyOutcome[],
): LatencyConfidenceScore {
  const latestT = outcomes.at(-1)?.tMs ?? 0;
  const window = outcomes.filter(
    (outcome) => outcome.tMs > latestT - LATENCY_CONFIDENCE_WINDOW_MS,
  );
  const values = window.flatMap((outcome) =>
    outcome.rttMs == null ? [] : [outcome.rttMs],
  );
  if (values.length < 2) {
    return {
      score: 0,
      jitterRatio: 1,
      lossRatio: 1,
      sampleCount: window.length,
    };
  }

  const center = median(values);
  const jitterMs = median(values.map((value) => Math.abs(value - center)));
  const jitterRatio = jitterMs / Math.max(center, LATENCY_JITTER_FLOOR_MS);
  const lossRatio = window.length
    ? window.filter((outcome) => outcome.rttMs === null).length / window.length
    : 0;

  const score = clamp(
    1 - jitterRatio * LATENCY_JITTER_K - lossRatio * LATENCY_LOSS_K,
    0,
    1,
  );

  return { score, jitterRatio, lossRatio, sampleCount: window.length };
}

/* ---------- Early-exit predicate ---------- */

interface ExitDecisionBase {
  /** ms elapsed within the current phase. */
  elapsedMs: number;
  /** nominal (configured) duration of the phase, ms. */
  durationMs: number;
  /** the relevant confidence score for this phase. */
  confidence: ConfidenceScore | LatencyConfidenceScore;
  /** adaptive config (coverage / stability / min-sample floors). */
  cfg: AdaptiveDurationConfig;
}

/** Latency decisions must name their cadence so no caller can silently bypass
 * cadence-aware evidence sizing. Transfer decisions have fixed control buckets. */
export type ExitDecisionInput = ExitDecisionBase &
  (
    | { kind: "latency"; latencyCadence: PingCadence }
    | { kind: "transfer"; latencyCadence?: never }
  );

/** The fraction of a phase's nominal duration that must elapse for an early
 *  exit: `minCoverageRatio`, and never below what `maxPhaseReductionRatio`
 *  permits, so the rail and the progress bar stay honest. */
function requiredCoverageRatio(cfg: AdaptiveDurationConfig): number {
  return Math.max(cfg.minCoverageRatio, 1 - cfg.maxPhaseReductionRatio);
}

function configuredFloor(value: number): number {
  return Number.isFinite(value) ? Math.max(0, Math.floor(value)) : 0;
}

/** Resolve the evidence floor from the actual phase policy. The configured
 * floor is the desired target; fixed source cadence, the trailing confidence
 * horizon, and the confirmation reserve cap it to a target that can arm before
 * the phase ends. Very short phases still retain a statistical minimum instead
 * of weakening confidence merely to force an early exit. */
type ConfidenceSampleFloorInput =
  | {
      kind: "latency";
      durationMs: number;
      cfg: AdaptiveDurationConfig;
      latencyCadence: PingCadence;
    }
  | {
      kind: "transfer";
      durationMs: number;
      cfg: AdaptiveDurationConfig;
    };

export function confidenceSampleFloor(
  input: ConfidenceSampleFloorInput,
): number {
  const { kind, cfg } = input;
  const requested = configuredFloor(
    kind === "latency" ? cfg.minLatencySamples : cfg.minTransferSamples,
  );
  if (requested === 0) return 0;

  const durationMs = Number.isFinite(input.durationMs)
    ? Math.max(0, input.durationMs)
    : 0;
  const confirmationMs = Number.isFinite(cfg.confirmationMs)
    ? Math.max(0, cfg.confirmationMs)
    : 0;
  const candidateBudgetMs = Math.max(0, durationMs - confirmationMs);
  let capacity: number;
  let statisticalMinimum: number;
  if (kind === "transfer") {
    capacity = Math.min(
      TRANSFER_CONFIDENCE_BUCKETS,
      Math.floor(candidateBudgetMs / TRANSFER_CONTROL_BUCKET_MS),
    );
    statisticalMinimum = MIN_TRANSFER_CONFIDENCE_SAMPLES;
  } else {
    const intervalMs = fixedPingIntervalMs(input.latencyCadence);
    // Reply-driven pacing reflects the path itself rather than an intentional
    // sampling delay. Do not lower its evidence target when the path yields too
    // few independent outcomes.
    if (intervalMs == null) return requested;
    capacity = Math.min(
      Math.ceil(LATENCY_CONFIDENCE_WINDOW_MS / intervalMs),
      1 + Math.floor(candidateBudgetMs / intervalMs),
    );
    statisticalMinimum = MIN_LATENCY_CONFIDENCE_SAMPLES;
  }

  const minimum = Math.min(requested, statisticalMinimum);
  return Math.max(minimum, Math.min(requested, capacity));
}

/**
 * True once the current phase may end early: the coverage floor is reached, the
 * stability score is at `stabilityThreshold`, and the kind's min-sample floor is
 * met. False while adaptive is disabled or the phase has no duration.
 */
export function shouldExitPhase(input: ExitDecisionInput): boolean {
  const { kind, elapsedMs, durationMs, confidence, cfg } = input;
  if (!cfg.enabled) return false;
  if (durationMs <= 0) return false;
  if (elapsedMs / durationMs < requiredCoverageRatio(cfg)) return false;
  if (confidence.score < cfg.stabilityThreshold) return false;

  const floor =
    kind === "latency"
      ? confidenceSampleFloor({
          kind,
          durationMs,
          cfg,
          latencyCadence: input.latencyCadence,
        })
      : confidenceSampleFloor({ kind, durationMs, cfg });
  return confidence.sampleCount >= floor;
}
