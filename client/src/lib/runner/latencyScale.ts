import type { LatencyBucket } from "./contract";
import { percentile } from "./stats";

const LATENCY_SCALE_WINDOW_MS = 6_000;
const LATENCY_SCALE_HEADROOM = 1.25;
export const LATENCY_SCALE_SHRINK_DWELL_MS = 2_000;
const LATENCY_SCALE_LADDER_MS = [
  20, 40, 100, 200, 400, 1_000, 2_000, 4_000,
];

function niceAbove(value: number): number {
  const exponent = 10 ** Math.floor(Math.log10(Math.max(1, value)));
  const normalized = value / exponent;
  const step =
    normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return step * exponent;
}

function tierFor(target: number): number {
  return (
    LATENCY_SCALE_LADDER_MS.find((tier) => tier >= target) ?? niceAbove(target)
  );
}

function scaleForMedians(medians: readonly number[]): number {
  const valid = medians.filter(
    (median) => Number.isFinite(median) && median >= 0,
  );
  if (!valid.length) return LATENCY_SCALE_LADDER_MS[0];
  return tierFor(percentile(valid, 95) * LATENCY_SCALE_HEADROOM);
}

/** Scale a gauge fallback that has a value but no presentation bucket yet. */
export function latencyScaleForReading(rttMs: number): number {
  return scaleForMedians([rttMs]);
}

/* Robust latency domain for a complete set of presentation buckets. */
export function latencyScaleForHistory(
  buckets: readonly LatencyBucket[],
): number {
  const medians = buckets.flatMap((bucket) =>
    bucket.medianRttMs == null || !Number.isFinite(bucket.medianRttMs)
      ? []
      : [bucket.medianRttMs],
  );
  return scaleForMedians(medians);
}

/* True when any visible part of a latency bucket exceeds the shared domain. */
export function latencyBucketExceedsScale(
  bucket: LatencyBucket,
  scaleMs: number,
): boolean {
  return [bucket.medianRttMs, bucket.p95RttMs, bucket.maxRttMs].some(
    (value) => value != null && value > scaleMs,
  );
}

/** Shared robust latency domain for the gauge and chart. */
export class LatencyScaleController {
  #history: {
    startT: number;
    endT: number;
    phase: LatencyBucket["phase"];
    underLoad: boolean;
    continuityId: number;
    median: number;
  }[] = [];
  #latestT = 0;
  #scaleMs = LATENCY_SCALE_LADDER_MS[0];
  #shrinkTarget = 0;
  #shrinkSince = 0;

  reset(): void {
    this.#history = [];
    this.#latestT = 0;
    this.#scaleMs = LATENCY_SCALE_LADDER_MS[0];
    this.#shrinkTarget = 0;
    this.#shrinkSince = 0;
  }

  observe(bucket: LatencyBucket): number {
    if (bucket.medianRttMs != null) {
      const entry = {
        startT: bucket.startT,
        endT: bucket.endT,
        phase: bucket.phase,
        underLoad: bucket.underLoad,
        continuityId: bucket.continuityId,
        median: bucket.medianRttMs,
      };
      const existing = this.#history.findIndex(
        (sample) =>
          sample.startT === entry.startT &&
          sample.phase === entry.phase &&
          sample.underLoad === entry.underLoad &&
          sample.continuityId === entry.continuityId,
      );
      if (existing >= 0) this.#history[existing] = entry;
      else this.#history.push(entry);
    }
    this.#latestT = Math.max(this.#latestT, bucket.endT);
    const latestT = this.#latestT;
    this.#history = this.#history.filter(
      (entry) => entry.endT > latestT - LATENCY_SCALE_WINDOW_MS,
    );
    const target = tierFor(
      percentile(
        this.#history.map((entry) => entry.median),
        95,
      ) * LATENCY_SCALE_HEADROOM,
    );
    if (target > this.#scaleMs) {
      this.#scaleMs = target;
      this.#shrinkTarget = 0;
      return this.#scaleMs;
    }
    if (target >= this.#scaleMs) {
      this.#shrinkTarget = 0;
      return this.#scaleMs;
    }
    const nextLower =
      [...LATENCY_SCALE_LADDER_MS]
        .filter((tier) => tier < this.#scaleMs && tier >= target)
        .at(-1) ?? target;
    if (this.#shrinkTarget !== nextLower) {
      this.#shrinkTarget = nextLower;
      this.#shrinkSince = latestT;
    } else if (latestT - this.#shrinkSince >= LATENCY_SCALE_SHRINK_DWELL_MS) {
      this.#scaleMs = nextLower;
      this.#shrinkTarget = 0;
    }
    return this.#scaleMs;
  }

  get scaleMs(): number {
    return this.#scaleMs;
  }
}
