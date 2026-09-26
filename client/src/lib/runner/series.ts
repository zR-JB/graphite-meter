import type { LatencyBucket, Phase, ThroughputSample } from "./contract";
import { nearestRank, sortedMedian } from "./measure";

/** Points kept per presented history; the producer keeps as many closed buckets for late revisions. */
export const SERIES_LIMIT = 1_200;
const BUCKET_MS = 200;
const SCALE_WINDOW_MS = 6_000;
const SCALE_HEADROOM = 1.25;
const SCALE_SHRINK_DWELL_MS = 2_000;
const SCALE_LADDER_MS = [20, 40, 100, 200, 400, 1_000, 2_000, 4_000];

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
    if (target !== pending) emitted.push(this.#summarize(target, target.endT));
    return emitted;
  }

  /** Closes buckets on the run's deadline even when no later ping arrives. */
  closeThrough(t: number): LatencyBucket[] {
    const emitted: LatencyBucket[] = [];
    while (this.#pending && t >= this.#pending.endT) {
      const pending = this.#pending;
      if (pending.pings) emitted.push(this.#summarize(pending, pending.endT));
      if (this.#closed.push(pending) > SERIES_LIMIT) this.#closed.shift();
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

  /** In place: a closed bucket stays sorted, so a late revision costs one adaptive pass. */
  #summarize(bucket: Bucket, endT: number): LatencyBucket {
    const rtts = bucket.rtts.sort((a, b) => a - b);
    return {
      t: (bucket.startT + endT) / 2,
      startT: bucket.startT,
      endT,
      medianRttMs: rtts.length ? sortedMedian(rtts) : null,
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
  const summary = { medianRttMs: value, p95RttMs: value, maxRttMs: value };
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

/** Inserts or revises a bucket in time order; true when existing points changed. */
export function upsertLatencyBucket(
  history: LatencyBucket[],
  bucket: LatencyBucket,
  limit = SERIES_LIMIT,
): boolean {
  const key = latencySeries(bucket);
  const existing = history.findIndex(
    (b) => b.startT === bucket.startT && latencySeries(b) === key,
  );
  const following = history.findIndex((b) => b.startT > bucket.startT);
  if (existing >= 0) history[existing] = bucket;
  else if (following >= 0) history.splice(following, 0, bucket);
  else history.push(bucket);
  if (history.length <= limit) return existing >= 0 || following >= 0;
  history.splice(
    0,
    history.length,
    ...compact(history, limit, latencySeries, (bin) => [mergeLatency(bin)]),
  );
  return true;
}

/** Replaces an equal-time point of the same series; only the timestamp tail can match. */
export function upsertThroughputSample(
  history: ThroughputSample[],
  sample: ThroughputSample,
): boolean {
  const key = throughputSeries(sample);
  for (let i = history.length - 1; i >= 0 && history[i].t === sample.t; i--)
    if (throughputSeries(history[i]) === key) {
      history[i] = sample;
      return true;
    }
  history.push(sample);
  return false;
}

/** True when existing history changed and incremental indexes must be rebuilt. */
export function appendThroughputSample(
  history: ThroughputSample[],
  sample: ThroughputSample,
  spanMs = 0,
): boolean {
  const replaced = upsertThroughputSample(history, sample);
  return (
    (history.length > SERIES_LIMIT &&
      compactThroughputHistory(history, spanMs)) ||
    replaced
  );
}

/** Keeps each series' first, last, and per-bin extremes, so compaction never hides a peak or a dip. */
export function compactThroughputHistory(
  history: ThroughputSample[],
  spanMs: number,
  limit = SERIES_LIMIT,
): boolean {
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
  if (
    reduced.length === history.length &&
    reduced.every((sample, i) => sample === history[i])
  )
    return false;
  history.splice(0, history.length, ...reduced);
  return true;
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
    const bins = new Map<string, T[]>();
    for (const value of history) {
      const key = `${series(value)}:${Math.floor(value.t / width)}`;
      const bin = bins.get(key);
      if (bin) bin.push(value);
      else bins.set(key, [value]);
    }
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
  const worst = (values: (number | null)[]) => {
    const finite = values.filter((value) => value != null);
    return finite.length ? Math.max(...finite) : null;
  };
  return {
    ...first,
    t: (first.startT + last.endT) / 2,
    endT: last.endT,
    medianRttMs: middle?.medianRttMs ?? null,
    p95RttMs: worst(bin.map((b) => b.p95RttMs)),
    maxRttMs: worst(bin.map((b) => b.maxRttMs)),
    pingCount: bin.reduce((sum, b) => sum + b.pingCount, 0),
    timeoutCount: bin.reduce((sum, b) => sum + b.timeoutCount, 0),
  };
}

/** The ladder tier above the p95 of reply medians, with headroom. */
export function latencyScale(medians: readonly (number | null)[]): number {
  const valid = medians
    .filter(
      (value): value is number =>
        value != null && Number.isFinite(value) && value >= 0,
    )
    .sort((a, b) => a - b);
  if (!valid.length) return SCALE_LADDER_MS[0];
  const target = nearestRank(valid, 0.95) * SCALE_HEADROOM;
  const tier = SCALE_LADDER_MS.find((value) => value >= target);
  if (tier) return tier;
  const exponent = 10 ** Math.floor(Math.log10(target));
  return [1, 2, 5, 10].find((step) => step * exponent >= target)! * exponent;
}

/** True when any drawn part of a bucket exceeds the domain. */
export function latencyBucketExceedsScale(
  bucket: LatencyBucket,
  scaleMs: number,
): boolean {
  return [bucket.medianRttMs, bucket.p95RttMs, bucket.maxRttMs].some(
    (value) => value != null && value > scaleMs,
  );
}

/** The live gauge and chart domain: grows at once, shrinks one tier after a dwell. */
export class LatencyScaleController {
  #recent: LatencyBucket[] = [];
  #latestT = 0;
  #scaleMs = SCALE_LADDER_MS[0];
  #shrinkTarget = 0;
  #shrinkSince = 0;

  reset(): void {
    this.#recent = [];
    this.#latestT = this.#shrinkTarget = this.#shrinkSince = 0;
    this.#scaleMs = SCALE_LADDER_MS[0];
  }

  observe(bucket: LatencyBucket): number {
    if (bucket.medianRttMs != null)
      upsertLatencyBucket(this.#recent, bucket, Infinity);
    const latestT = (this.#latestT = Math.max(this.#latestT, bucket.endT));
    this.#recent = this.#recent.filter(
      (b) => b.endT > latestT - SCALE_WINDOW_MS,
    );
    const target = latencyScale(this.#recent.map((b) => b.medianRttMs));
    if (target >= this.#scaleMs) {
      this.#scaleMs = target;
      this.#shrinkTarget = 0;
      return this.#scaleMs;
    }
    const lower =
      SCALE_LADDER_MS.findLast(
        (tier) => tier < this.#scaleMs && tier >= target,
      ) ?? target;
    if (this.#shrinkTarget !== lower) {
      this.#shrinkTarget = lower;
      this.#shrinkSince = latestT;
    } else if (latestT - this.#shrinkSince >= SCALE_SHRINK_DWELL_MS) {
      this.#scaleMs = lower;
      this.#shrinkTarget = 0;
    }
    return this.#scaleMs;
  }

  get scaleMs(): number {
    return this.#scaleMs;
  }
}
