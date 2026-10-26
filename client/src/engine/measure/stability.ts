// Cuts rates into buckets, measures steadiness and applies the early-finish rule (docs/MEASUREMENTS.md#stage-timing).

export const BUCKET_MS = 250;
/** Early finish scores this trailing window. */
export const SCORE_WINDOW_MS = 4_000;
export const SCORE_BUCKETS = SCORE_WINDOW_MS / BUCKET_MS;

/** Keeps running statistics one value at a time (Welford), exact for any spread at any level. */
export interface Moments {
  count: number;
  mean: number;
  squares: number;
}

/** `restore` takes a series back to these moments over every rate and over the window open then. */
export interface Mark {
  all: Moments;
  window: Moments | null;
}

const fresh = (): Moments => ({ count: 0, mean: 0, squares: 0 });

/** Fixed 250 ms rates from byte and time evidence, with running moments over all and over an open window, and the latest `keep`. */
export class RateSeries {
  readonly recent: number[] = [];
  #bytes = 0;
  #ms = 0;
  #all = fresh();
  #window: Moments | null = null;
  constructor(readonly keep = 0) {}

  get mark(): Mark {
    return { all: { ...this.#all }, window: this.#window && { ...this.#window } };
  }

  restore(mark: Mark): void {
    this.#all = { ...mark.all };
    this.#window = mark.window && { ...mark.window };
  }

  /** Starts a window's moments from the next rate on. */
  openWindow(): void {
    this.#window = fresh();
  }

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
      this.#complete((this.#bytes * 1000) / BUCKET_MS);
      this.#bytes = this.#ms = 0;
    }
  }

  #complete(rate: number): void {
    add(this.#all, rate);
    if (this.#window) add(this.#window, rate);
    if (!this.keep) return;
    this.recent.push(rate);
    if (this.recent.length > this.keep) this.recent.shift();
  }

  /** The open window's rate count, and its mean's standard error as a share of that mean. */
  get windowError(): { count: number; relative: number } {
    const m = this.#window;
    if (!m || m.count < 2 || !(m.mean > 0)) return { count: m?.count ?? 0, relative: Infinity };
    return { count: m.count, relative: Math.sqrt(m.squares / (m.count - 1) / m.count) / m.mean };
  }

  /** Reads the stabilityPct of every 250 ms rate, or of the open window's. */
  stability(window = false): number {
    const m = window ? (this.#window ?? fresh()) : this.#all;
    if (m.count < 2 || !(m.mean > 0)) return 0;
    return Math.max(0, 1 - Math.sqrt(m.squares / m.count) / m.mean) * 100;
  }
}

export function add(m: Moments, rate: number): void {
  m.count++;
  const delta = rate - m.mean;
  m.mean += delta / m.count;
  m.squares += delta * (rate - m.mean);
}

export interface Confidence {
  score: number;
  sampleCount: number;
  /** Holds the coefficient of variation for a transfer. */
  variation?: number;
}

const mean = (values: readonly number[]) =>
  values.length ? values.reduce((a, b) => a + b, 0) / values.length : 0;
const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/** 1 − 2.2 × CV − 1.4 × |last third − first third| / mean over the trailing buckets. */
export function transferConfidence(rates: readonly number[], buckets = SCORE_BUCKETS): Confidence {
  const values = rates.slice(-buckets);
  const average = mean(values);
  if (values.length < 2 || average <= 0)
    return { score: 0, variation: 1, sampleCount: values.length };
  const variation = Math.sqrt(mean(values.map((value) => (value - average) ** 2))) / average;
  const third = Math.max(2, Math.ceil(values.length / 3));
  const slope = Math.abs(mean(values.slice(-third)) - mean(values.slice(0, third))) / average;
  return {
    score: clamp01(1 - variation * 2.2 - slope * 1.4),
    variation,
    sampleCount: values.length,
  };
}

/** Returns 100 × (1 − CV) of every bucket, holes included, floored at 0, and 0 below two buckets. */
export function stabilityPct(rates: readonly number[]): number {
  const { sampleCount, variation = 1 } = transferConfidence(rates, rates.length);
  return sampleCount >= 2 ? Math.max(0, 1 - variation) * 100 : 0;
}

export const EARLY_FINISH = {
  /** An early finish ends a transfer once its steady window's mean has this standard error, as a share of the mean. */
  standardError: 0.0025,
  /** This share of the latency stage must have run. */
  minCoverage: 0.52,
  stability: 0.86,
  latencySamples: 8,
  transferSamples: 12,
} as const;

/** A complete transfer stage's intervals must span this share of its planned time; an early finish's steady window
 *  promises its own accuracy instead. */
export const minCoverage = (early: boolean) => (early ? 0 : 0.75);

/** Acts as a Schmitt trigger, stable from the threshold and unstable again 0.08 below it. */
export const stillStable = (wasStable: boolean, score: number) =>
  score >= EARLY_FINISH.stability - (wasStable ? 0.08 : 0);

/** The replies a latency stage can reach within its time, never below three; `intervalMs` null is reply-driven. */
function replyFloor(durationMs: number, intervalMs: number | null): number {
  const requested = EARLY_FINISH.latencySamples;
  if (!intervalMs) return requested;
  const budgetMs = Math.max(0, Number.isFinite(durationMs) ? durationMs : 0);
  const capacity = Math.min(
    Math.ceil(SCORE_WINDOW_MS / intervalMs),
    1 + Math.floor(budgetMs / intervalMs),
  );
  return Math.max(Math.min(requested, 3), Math.min(requested, capacity));
}

/** The latency stage may finish early once it covers its share with a stable score and enough replies. */
export function latencyMayFinish(input: {
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
    confidence.sampleCount >= replyFloor(durationMs, input.intervalMs)
  );
}

/** A transfer stage may finish early once its steady window holds enough rates and its mean's standard error is within
 *  the promise; fewer rates would understate their own spread. */
export const transferMayFinish = (steady: { count: number; relative: number }) =>
  steady.count >= EARLY_FINISH.transferSamples && steady.relative < EARLY_FINISH.standardError;
