// Rate buckets, steadiness and the early-finish rule (docs/MEASUREMENTS.md#stage-timing).

export const BUCKET_MS = 250;
/** The trailing window early finish scores. */
export const SCORE_WINDOW_MS = 4_000;
const SCORE_BUCKETS = SCORE_WINDOW_MS / BUCKET_MS;

/** Byte and time evidence cut into fixed 250 ms rate buckets, at most `limit` recent ones. */
export class RateBuckets {
  readonly rates: number[] = [];
  #bytes = 0;
  #ms = 0;
  constructor(readonly limit = Infinity) {}

  observe(bytes: number, ms: number): void {
    if (!(ms > 0) || !Number.isFinite(ms)) return;
    bytes = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
    while (ms > 0) {
      const take = Math.min(BUCKET_MS - this.#ms, ms);
      const part = (bytes * take) / ms;
      this.#bytes += part;
      this.#ms += take;
      bytes -= part;
      ms -= take;
      if (this.#ms < BUCKET_MS - 1e-9) continue;
      this.rates.push((this.#bytes * 1000) / BUCKET_MS);
      if (this.rates.length > this.limit) this.rates.shift();
      this.#bytes = this.#ms = 0;
    }
  }
}

export interface Confidence {
  score: number;
  sampleCount: number;
  /** The coefficient of variation, for a transfer. */
  variation?: number;
}

const mean = (values: readonly number[]) => (values.length ? values.reduce((a, b) => a + b, 0) / values.length : 0);
const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/** 1 − 2.2 × CV − 1.4 × |last third − first third| / mean over the trailing buckets. */
export function transferConfidence(rates: readonly number[], buckets = SCORE_BUCKETS): Confidence {
  const values = rates.slice(-buckets);
  const average = mean(values);
  if (values.length < 2 || average <= 0) return { score: 0, variation: 1, sampleCount: values.length };
  const variation = Math.sqrt(mean(values.map((value) => (value - average) ** 2))) / average;
  const third = Math.max(2, Math.ceil(values.length / 3));
  const slope = Math.abs(mean(values.slice(-third)) - mean(values.slice(0, third))) / average;
  return { score: clamp01(1 - variation * 2.2 - slope * 1.4), variation, sampleCount: values.length };
}

/** 100 × (1 − CV) of every bucket, holes included, floored at 0; 0 below two buckets. */
export function stabilityPct(rates: readonly number[]): number {
  const { sampleCount, variation = 1 } = transferConfidence(rates, rates.length);
  return sampleCount >= 2 ? Math.max(0, 1 - variation) * 100 : 0;
}

export const EARLY_FINISH = {
  /** The share of a stage that must have run. */
  minCoverage: 0.52,
  stability: 0.86,
  latencySamples: 8,
  transferSamples: 12,
  /** How long the score must hold. */
  confirmationMs: 1_100,
} as const;

/** A complete transfer stage's intervals must span this share of its planned time. */
export const minCoverage = (early: boolean) => (early ? EARLY_FINISH.minCoverage : 0.75);

/** A Schmitt trigger: stable from the threshold, unstable again 0.08 below it. */
export const stillStable = (wasStable: boolean, score: number) =>
  score >= EARLY_FINISH.stability - (wasStable ? 0.08 : 0);

/** The evidence a stage can reach within its time, never below the statistical minimum. `intervalMs` is the
    fixed probe spacing of a latency stage, null when reply-driven. */
export function sampleFloor(kind: "latency" | "transfer", durationMs: number, intervalMs: number | null): number {
  const requested = kind === "latency" ? EARLY_FINISH.latencySamples : EARLY_FINISH.transferSamples;
  const budgetMs = Math.max(0, (Number.isFinite(durationMs) ? durationMs : 0) - EARLY_FINISH.confirmationMs);
  const spacing = kind === "latency" ? intervalMs : BUCKET_MS;
  if (!spacing) return requested;
  const capacity =
    kind === "latency"
      ? Math.min(Math.ceil(SCORE_WINDOW_MS / spacing), 1 + Math.floor(budgetMs / spacing))
      : Math.min(SCORE_BUCKETS, Math.floor(budgetMs / spacing));
  return Math.max(Math.min(requested, kind === "latency" ? 3 : 4), Math.min(requested, capacity));
}

/** An early finish needs the coverage, a stable score and enough evidence. */
export function mayFinishEarly(input: {
  kind: "latency" | "transfer";
  intervalMs: number | null;
  elapsedMs: number;
  durationMs: number;
  confidence: Confidence;
}): boolean {
  const { durationMs, confidence } = input;
  return (
    durationMs > 0 &&
    input.elapsedMs / durationMs >= EARLY_FINISH.minCoverage &&
    confidence.score >= EARLY_FINISH.stability &&
    confidence.sampleCount >= sampleFloor(input.kind, durationMs, input.intervalMs)
  );
}
