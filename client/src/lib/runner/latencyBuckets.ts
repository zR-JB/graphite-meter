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
    const emitted: LatencyBucket[] = [];
    while (t >= this.#pending!.endT) {
      const closed = this.#emitPending(this.#pending!.endT);
      if (closed) emitted.push(closed);
      this.#pending = this.#empty(this.#pending!.endT);
    }
    this.#pending!.pingCount++;
    if (lost) this.#pending!.lossCount++;
    else if (Number.isFinite(rttMs))
      this.#pending!.rtts.push(Math.max(0, rttMs));
    return emitted;
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
    return {
      t: pending.startT + (endT - pending.startT) / 2,
      startT: pending.startT,
      endT,
      medianRttMs: pending.rtts.length ? median(pending.rtts) : null,
      p95RttMs: pending.rtts.length ? percentile(pending.rtts, 95) : null,
      maxRttMs: pending.rtts.length ? Math.max(...pending.rtts) : null,
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
    pingCount: 1,
    lossCount: lost ? 1 : 0,
    underLoad: false,
    phase,
    continuityId: 0,
  };
}
