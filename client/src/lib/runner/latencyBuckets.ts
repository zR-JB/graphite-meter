import type { LatencyBucket, Phase } from "./contract";
import { median, percentile } from "./stats";

export const LATENCY_PRESENTATION_BUCKET_MS = 200;

interface PendingBucket {
  startT: number;
  endT: number;
  rtts: number[];
  pingCount: number;
  lossCount: number;
}

export class LatencyPresentationBuckets {
  #phase: Phase = "idle";
  #underLoad = false;
  #continuityId = 0;
  #pending: PendingBucket | null = null;

  reset(
    phaseStartT: number,
    phase: Phase,
    underLoad: boolean,
    continuityId: number,
  ): void {
    this.#phase = phase;
    this.#underLoad = underLoad;
    this.#continuityId = continuityId;
    this.#pending = this.#empty(phaseStartT);
  }

  observe(t: number, rttMs: number, lost: boolean): LatencyBucket[] {
    if (!this.#pending)
      this.reset(t, this.#phase, this.#underLoad, this.#continuityId);
    const emitted = this.closeThrough(t);
    this.#pending!.pingCount++;
    if (lost) this.#pending!.lossCount++;
    else if (Number.isFinite(rttMs))
      this.#pending!.rtts.push(Math.max(0, rttMs));
    return emitted;
  }

  /** Close every elapsed presentation window. The runner's master deadline
   *  calls this even when no later ping arrives, so display cadence is owned by
   *  bucket time rather than source cadence. */
  closeThrough(t: number): LatencyBucket[] {
    const emitted: LatencyBucket[] = [];
    while (this.#pending && t >= this.#pending.endT) {
      const endT = this.#pending.endT;
      const closed = this.#emitPending(endT);
      if (closed) emitted.push(closed);
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
    const bucket = this.#emitPending(endT);
    this.#pending = null;
    return bucket;
  }

  #empty(startT: number): PendingBucket {
    return {
      startT,
      endT: startT + LATENCY_PRESENTATION_BUCKET_MS,
      rtts: [],
      pingCount: 0,
      lossCount: 0,
    };
  }

  #emitPending(endT: number): LatencyBucket | null {
    const pending = this.#pending;
    if (!pending || pending.pingCount === 0) return null;
    let rttDeltaSumMs = 0;
    for (let i = 1; i < pending.rtts.length; i++)
      rttDeltaSumMs += Math.abs(pending.rtts[i] - pending.rtts[i - 1]);
    return {
      t: pending.startT + (endT - pending.startT) / 2,
      startT: pending.startT,
      endT,
      medianRttMs: pending.rtts.length ? median(pending.rtts) : null,
      p95RttMs: pending.rtts.length ? percentile(pending.rtts, 95) : null,
      maxRttMs: pending.rtts.length ? Math.max(...pending.rtts) : null,
      firstRttMs: pending.rtts.at(0) ?? null,
      lastRttMs: pending.rtts.at(-1) ?? null,
      rttDeltaSumMs,
      rttDeltaCount: Math.max(0, pending.rtts.length - 1),
      pingCount: pending.pingCount,
      lossCount: pending.lossCount,
      underLoad: this.#underLoad,
      phase: this.#phase,
      continuityId: this.#continuityId,
    };
  }
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

/** Exact mean absolute consecutive RTT difference across summarized buckets.
 *  Losses carry no RTT and are skipped, matching the former raw-outcome view. */
export function latencyJitterMs(buckets: readonly LatencyBucket[]): number {
  let previousRtt: number | null = null;
  let deltaSumMs = 0;
  let deltaCount = 0;
  for (const bucket of buckets) {
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
