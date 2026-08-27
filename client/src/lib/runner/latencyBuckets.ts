import type { LatencyBucket, Phase } from "./contract";
import { median, percentile } from "./stats";
import {
  compactLatencyHistory,
  PRESENTATION_POINT_LIMIT,
} from "./presentationHistory";

export const LATENCY_PRESENTATION_BUCKET_MS = 200;
/* Keep the same bounded history in the producer and store so a delayed worker delivery can revise any bucket the. */
const LATENCY_PRESENTATION_HISTORY_LIMIT = 1_200;

export function latencyPresentationBucketMs(durationMs: number): number {
  const minimum = LATENCY_PRESENTATION_BUCKET_MS;
  const needed = Math.max(0, durationMs) / PRESENTATION_POINT_LIMIT;
  return Math.max(minimum, Math.ceil(needed / minimum) * minimum);
}

interface TimedRtt {
  t: number;
  value: number;
  sequence: number;
}

interface PendingBucket {
  startT: number;
  endT: number;
  rtts: TimedRtt[];
  pingCount: number;
  lossCount: number;
}

export class LatencyPresentationBuckets {
  #phase: Phase = "idle";
  #underLoad = false;
  #continuityId = 0;
  #pending: PendingBucket | null = null;
  #closed: PendingBucket[] = [];
  #sequence = 0;
  #bucketMs = LATENCY_PRESENTATION_BUCKET_MS;

  reset(
    phaseStartT: number,
    phase: Phase,
    underLoad: boolean,
    continuityId: number,
    durationMs = 0,
  ): void {
    this.#phase = phase;
    this.#underLoad = underLoad;
    this.#continuityId = continuityId;
    this.#bucketMs = latencyPresentationBucketMs(durationMs);
    this.#pending = this.#empty(phaseStartT);
    this.#closed = [];
    this.#sequence = 0;
  }

  widen(durationMs: number): void {
    const bucketMs = latencyPresentationBucketMs(durationMs);
    if (bucketMs <= this.#bucketMs) return;
    this.#bucketMs = bucketMs;
    if (this.#pending)
      this.#pending.endT = this.#pending.startT + this.#bucketMs;
  }

  observe(t: number, rttMs: number, lost: boolean): LatencyBucket[] {
    if (!this.#pending)
      this.reset(t, this.#phase, this.#underLoad, this.#continuityId);
    const emitted = this.closeThrough(t);
    const pending = this.#pending!;
    const target =
      t >= pending.startT
        ? pending
        : this.#closed.findLast(
            (bucket) => t >= bucket.startT && t < bucket.endT,
          );
// The raw accumulator already owns the outcome.
    if (!target) return emitted;
    this.#record(target, t, rttMs, lost);
    if (target !== pending) {
      const revised = this.#summarize(target, target.endT);
      if (revised) emitted.push(revised);
    }
    return emitted;
  }

/* The runner's master deadline calls this even when no later ping arrives, so display cadence is owned by bucket. */
  closeThrough(t: number): LatencyBucket[] {
    const emitted: LatencyBucket[] = [];
    while (this.#pending && t >= this.#pending.endT) {
      const pending = this.#pending;
      const endT = pending.endT;
      const closed = this.#summarize(pending, endT);
      if (closed) emitted.push(closed);
      this.#closed.push(pending);
      if (this.#closed.length > LATENCY_PRESENTATION_HISTORY_LIMIT)
        this.#closed.shift();
      this.#pending = this.#empty(endT);
    }
    return emitted;
  }

  get nextBoundaryT(): number | null {
    return this.#pending?.endT ?? null;
  }

  flush(atT?: number): LatencyBucket | null {
    if (!this.#pending) return null;
    const endT = Math.max(
      this.#pending.startT,
      Math.min(this.#pending.endT, atT ?? this.#pending.endT),
    );
    const bucket = this.#summarize(this.#pending, endT);
    this.#pending = null;
    return bucket;
  }

  #empty(startT: number): PendingBucket {
    return {
      startT,
      endT: startT + this.#bucketMs,
      rtts: [],
      pingCount: 0,
      lossCount: 0,
    };
  }

  #record(
    bucket: PendingBucket,
    t: number,
    rttMs: number,
    lost: boolean,
  ): void {
    bucket.pingCount++;
    if (lost) bucket.lossCount++;
    else if (Number.isFinite(rttMs))
      bucket.rtts.push({
        t,
        value: Math.max(0, rttMs),
        sequence: this.#sequence++,
      });
  }

  #summarize(pending: PendingBucket, endT: number): LatencyBucket | null {
    if (pending.pingCount === 0) return null;
    const rtts = [...pending.rtts]
      .sort((a, b) => a.t - b.t || a.sequence - b.sequence)
      .map((sample) => sample.value);
    let rttDeltaSumMs = 0;
    for (let i = 1; i < rtts.length; i++)
      rttDeltaSumMs += Math.abs(rtts[i] - rtts[i - 1]);
    return {
      t: pending.startT + (endT - pending.startT) / 2,
      startT: pending.startT,
      endT,
      medianRttMs: rtts.length ? median(rtts) : null,
      p95RttMs: rtts.length ? percentile(rtts, 95) : null,
      maxRttMs: rtts.length ? Math.max(...rtts) : null,
      firstRttMs: rtts.at(0) ?? null,
      lastRttMs: rtts.at(-1) ?? null,
      rttDeltaSumMs,
      rttDeltaCount: Math.max(0, rtts.length - 1),
      pingCount: pending.pingCount,
      lossCount: pending.lossCount,
      underLoad: this.#underLoad,
      phase: this.#phase,
      continuityId: this.#continuityId,
    };
  }
}

function sameLatencyWindow(a: LatencyBucket, b: LatencyBucket): boolean {
  return (
    a.startT === b.startT &&
    a.phase === b.phase &&
    a.underLoad === b.underLoad &&
    a.continuityId === b.continuityId
  );
}

type LatencyHistoryMutation = "tail-append" | "structural-change";

/* Insert a newly closed bucket or replace a late revision in chronological order. */
export function upsertLatencyBucket(
  history: LatencyBucket[],
  bucket: LatencyBucket,
  limit = LATENCY_PRESENTATION_HISTORY_LIMIT,
): LatencyHistoryMutation {
  let mutation: LatencyHistoryMutation = "tail-append";
  const existing = history.findIndex((sample) =>
    sameLatencyWindow(sample, bucket),
  );
  if (existing >= 0) {
    history[existing] = bucket;
    mutation = "structural-change";
  } else {
    const following = history.findIndex(
      (sample) => sample.startT > bucket.startT,
    );
    if (following < 0) history.push(bucket);
    else {
      history.splice(following, 0, bucket);
      mutation = "structural-change";
    }
  }
  if (history.length > Math.max(0, limit)) {
    compactLatencyHistory(history, limit);
    mutation = "structural-change";
  }
  return mutation;
}

export function singleLatencyBucket(
  t: number,
  rttMs: number,
  lost: boolean,
  phase: Phase = "idle",
): LatencyBucket {
  const value = lost ? null : Math.max(0, rttMs);
  return {
    t,
    startT: t,
    endT: t,
    medianRttMs: value,
    p95RttMs: value,
    maxRttMs: value,
    firstRttMs: value,
    lastRttMs: value,
    rttDeltaSumMs: 0,
    rttDeltaCount: 0,
    pingCount: 1,
    lossCount: lost ? 1 : 0,
    underLoad: false,
    phase,
    continuityId: 0,
  };
}

/* Losses carry no RTT and are skipped; explicit phase/stall continuity breaks never create a synthetic. */
export function latencyJitterMs(buckets: readonly LatencyBucket[]): number {
  let previousRtt: number | null = null;
  let previousBucket: LatencyBucket | null = null;
  let deltaSumMs = 0;
  let deltaCount = 0;
  for (const bucket of buckets) {
    if (
      previousBucket &&
      (bucket.continuityId !== previousBucket.continuityId ||
        bucket.phase !== previousBucket.phase ||
        bucket.underLoad !== previousBucket.underLoad)
    )
      previousRtt = null;
    previousBucket = bucket;
    if (bucket.firstRttMs == null) continue;
    if (previousRtt != null) {
      deltaSumMs += Math.abs(bucket.firstRttMs - previousRtt);
      deltaCount++;
    }
    deltaSumMs += bucket.rttDeltaSumMs;
    deltaCount += bucket.rttDeltaCount;
    previousRtt = bucket.lastRttMs;
  }
  return deltaCount > 0 ? deltaSumMs / deltaCount : 0;
}
