import type { LatencyBucket, Phase, ThroughputSample } from "./contract";
import { nearestRank, sortedMedian } from "./measure";

/** Points kept per presented history. */
export const SERIES_LIMIT = 1_200;
const BUCKET_MS = 200;
const REVISION_MS = 10_000;
const NONE: LatencyBucket[] = [];

type Bucket = {
  startT: number;
  endT: number;
  rtts: number[];
  pings: number;
  timeouts: number;
};

/** Fixed-cadence widths align to the ping interval, so no bin is empty by construction. */
export function latencyBucketMs(
  durationMs: number,
  pingIntervalMs: number | null = null,
): number {
  const needed = Math.max(0, durationMs) / SERIES_LIMIT;
  const base = Math.max(BUCKET_MS, Math.ceil(needed / BUCKET_MS) * BUCKET_MS);
  return pingIntervalMs && pingIntervalMs > 0
    ? Math.ceil(base / pingIntervalMs) * pingIntervalMs
    : base;
}

/** Groups one server's probe outcomes into time buckets; a timeout counts as a ping, never as an RTT point. */
export class LatencyPresentationBuckets {
  #phase: Phase = "idle";
  #underLoad = false;
  #continuityId = 0;
  #bucketMs = BUCKET_MS;
  #pingIntervalMs: number | null = null;
  #pending: Bucket | null = null;
  #closed: Bucket[] = [];

  reset(
    startT: number,
    phase: Phase,
    underLoad: boolean,
    continuityId: number,
    durationMs = 0,
    pingIntervalMs: number | null = null,
  ): void {
    this.#phase = phase;
    this.#underLoad = underLoad;
    this.#pingIntervalMs = pingIntervalMs;
    this.#bucketMs = latencyBucketMs(durationMs, pingIntervalMs);
    this.restart(startT, continuityId);
  }

  /** A new presentation series from `t`, keeping the phase and bucket width. */
  restart(t: number, continuityId: number): void {
    this.#continuityId = continuityId;
    this.#pending = this.#empty(t);
    this.#closed = [];
  }

  widen(durationMs: number): void {
    this.#bucketMs = Math.max(
      this.#bucketMs,
      latencyBucketMs(durationMs, this.#pingIntervalMs),
    );
    if (this.#pending)
      this.#pending.endT = this.#pending.startT + this.#bucketMs;
  }

  /** A late outcome revises the closed bucket that owns its time. */
  observe(t: number, rttMs: number, timedOut: boolean): LatencyBucket[] {
    this.#pending ??= this.#empty(t);
    const emitted = this.closeThrough(t);
    const pending = this.#pending!;
    const target =
      t >= pending.startT
        ? pending
        : this.#closed.findLast((b) => t >= b.startT && t < b.endT);
    if (!target) return emitted;
    target.pings++;
    if (timedOut) target.timeouts++;
    else if (Number.isFinite(rttMs)) target.rtts.push(Math.max(0, rttMs));
    return target === pending
      ? emitted
      : [...emitted, this.#summarize(target, target.endT)];
  }

  /** Closes buckets on the run's deadline even when no later ping arrives; never mutate the result. */
  closeThrough(t: number): LatencyBucket[] {
    let emitted = NONE;
    while (this.#pending && t >= this.#pending.endT) {
      // A suspended page may return hours later. Only the revision horizon can still accept late outcomes.
      if (!this.#pending.pings) {
        const skipped = Math.floor(
          (t - this.#pending.startT - REVISION_MS) / this.#bucketMs,
        );
        if (skipped > 0)
          this.#pending = this.#empty(
            this.#pending.startT + skipped * this.#bucketMs,
          );
      }
      const pending = this.#pending;
      if (pending.pings)
        emitted = [...emitted, this.#summarize(pending, pending.endT)];
      this.#closed.push(pending);
      while (this.#closed[0].endT < pending.endT - REVISION_MS)
        this.#closed.shift();
      this.#pending = this.#empty(pending.endT);
    }
    return emitted;
  }

  get nextBoundaryT(): number | null {
    return this.#pending?.endT ?? null;
  }

  flush(atT?: number): LatencyBucket | null {
    const pending = this.#pending;
    this.#pending = null;
    this.#closed = [];
    if (!pending?.pings) return null;
    return this.#summarize(
      pending,
      Math.max(pending.startT, Math.min(pending.endT, atT ?? pending.endT)),
    );
  }

  #empty(startT: number): Bucket {
    return {
      startT,
      endT: startT + this.#bucketMs,
      rtts: [],
      pings: 0,
      timeouts: 0,
    };
  }

  #summarize(bucket: Bucket, endT: number): LatencyBucket {
    const rtts = bucket.rtts.sort((a, b) => a - b);
    return {
      t: (bucket.startT + endT) / 2,
      startT: bucket.startT,
      endT,
      medianRttMs: rtts.length ? sortedMedian(rtts) : null,
      minRttMs: rtts[0] ?? null,
      p95RttMs: rtts.length ? nearestRank(rtts, 0.95) : null,
      maxRttMs: rtts.at(-1) ?? null,
      pingCount: bucket.pings,
      timeoutCount: bucket.timeouts,
      underLoad: this.#underLoad,
      phase: this.#phase,
      continuityId: this.#continuityId,
    };
  }
}

export function singleLatencyBucket(
  t: number,
  rttMs: number,
  timedOut: boolean,
  phase: Phase = "idle",
): LatencyBucket {
  const value = timedOut ? null : Math.max(0, rttMs);
  const summary = {
    medianRttMs: value,
    minRttMs: value,
    p95RttMs: value,
    maxRttMs: value,
  };
  return {
    t,
    startT: t,
    endT: t,
    ...summary,
    pingCount: 1,
    timeoutCount: timedOut ? 1 : 0,
    underLoad: false,
    phase,
    continuityId: 0,
  };
}

const latencySeries = (b: LatencyBucket) =>
  `${b.phase}:${b.underLoad}:${b.continuityId}`;
const throughputSeries = (s: ThroughputSample) =>
  `${s.phase}:${s.dir}:${s.continuityId}`;

/** Inserts or revises a bucket, keeping start order through compaction. */
export function upsertLatencyBucket(
  history: LatencyBucket[],
  bucket: LatencyBucket,
  limit = SERIES_LIMIT,
): void {
  const key = latencySeries(bucket);
  let at = history.length;
  while (at > 0 && history[at - 1].startT > bucket.startT) at--;
  let same = at - 1;
  while (
    same >= 0 &&
    history[same].startT === bucket.startT &&
    latencySeries(history[same]) !== key
  )
    same--;
  if (same >= 0 && history[same].startT === bucket.startT)
    history[same] = bucket;
  else history.splice(at, 0, bucket);
  if (history.length <= limit) return;
  const reduced = compact(history, limit, latencySeries, (bin) => [
    mergeLatency(bin),
  ]);
  history.splice(
    0,
    history.length,
    ...reduced.sort((a, b) => a.startT - b.startT),
  );
}

/** Replaces an equal-time point of the same series; only the timestamp tail can match. */
function upsertThroughputSample(
  history: ThroughputSample[],
  sample: ThroughputSample,
): void {
  const key = throughputSeries(sample);
  for (let i = history.length - 1; i >= 0 && history[i].t === sample.t; i--)
    if (throughputSeries(history[i]) === key) {
      history[i] = sample;
      return;
    }
  history.push(sample);
}

export function appendThroughputSample(
  history: ThroughputSample[],
  sample: ThroughputSample,
  spanMs = 0,
): void {
  upsertThroughputSample(history, sample);
  if (history.length > SERIES_LIMIT) compactThroughputHistory(history, spanMs);
}

/** Keeps each series' first, last, and per-bin extremes, so compaction never hides a peak or a dip. */
export function compactThroughputHistory(
  history: ThroughputSample[],
  spanMs: number,
  limit = SERIES_LIMIT,
): void {
  const canonical: ThroughputSample[] = [];
  for (const sample of history) upsertThroughputSample(canonical, sample);
  const reduced = compact(
    canonical,
    limit,
    throughputSeries,
    extremes,
    spanMs,
    4,
  );
  history.splice(0, history.length, ...reduced);
}

function extremes(bin: readonly ThroughputSample[]): ThroughputSample[] {
  const low = bin.reduce((a, b) => (b.bytesPerSec < a.bytesPerSec ? b : a));
  const high = bin.reduce((a, b) => (b.bytesPerSec > a.bytesPerSec ? b : a));
  return [...new Set([bin[0], low, high, bin.at(-1)!])];
}

/** Doubles the bin width per series until the reduction fits, then samples evenly as a last resort. */
function compact<T extends { t: number }>(
  history: readonly T[],
  limit: number,
  series: (value: T) => string,
  merge: (bin: T[]) => T[],
  spanMs = 0,
  pointsPerBin = 1,
): T[] {
  if (history.length <= 1 || limit <= 0)
    return history.length > limit ? [] : [...history];
  const count = new Set(history.map(series)).size;
  const span = Math.max(1, spanMs, history.at(-1)!.t - history[0].t);
  let width = Math.max(
    1,
    span / Math.max(1, Math.floor(limit / (count * pointsPerBin))),
  );
  let reduced: T[];
  do {
    const bins = Map.groupBy(
      history,
      (value) => `${series(value)}:${Math.floor(value.t / width)}`,
    );
    reduced = [...bins.values()].flatMap(merge).sort((a, b) => a.t - b.t);
    width *= 2;
  } while (reduced.length > limit && width / 2 < span);
  if (reduced.length <= limit) return reduced;
  if (limit === 1) return [reduced.at(-1)!];
  return Array.from(
    { length: limit },
    (_, i) => reduced[Math.round((i * (reduced.length - 1)) / (limit - 1))],
  );
}

/** Merged buckets keep the success-weighted median and the worst tail. */
function mergeLatency(bin: LatencyBucket[]): LatencyBucket {
  const first = bin[0];
  const last = bin.at(-1)!;
  const replies = bin
    .filter((b) => b.medianRttMs != null)
    .sort((a, b) => a.medianRttMs! - b.medianRttMs!);
  const successes = replies.reduce(
    (sum, b) => sum + b.pingCount - b.timeoutCount,
    0,
  );
  let seen = 0;
  const middle = replies.find(
    (b) => (seen += b.pingCount - b.timeoutCount) >= successes / 2,
  );
  const finite = (values: (number | null)[]) =>
    values.filter((value) => value != null);
  const worst = (values: (number | null)[]) => {
    const known = finite(values);
    return known.length ? Math.max(...known) : null;
  };
  const least = (values: (number | null)[]) => {
    const known = finite(values);
    return known.length ? Math.min(...known) : null;
  };
  return {
    ...first,
    t: (first.startT + last.endT) / 2,
    endT: last.endT,
    medianRttMs: middle?.medianRttMs ?? null,
    minRttMs: least(bin.map((b) => b.minRttMs)),
    p95RttMs: worst(bin.map((b) => b.p95RttMs)),
    maxRttMs: worst(bin.map((b) => b.maxRttMs)),
    pingCount: bin.reduce((sum, b) => sum + b.pingCount, 0),
    timeoutCount: bin.reduce((sum, b) => sum + b.timeoutCount, 0),
  };
}
