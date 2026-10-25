// Groups a server's probe outcomes into fixed time buckets for display; late outcomes revise the last ten seconds.
import type { Stage } from "../run/schedule";
import { midpointMedian, nearestRank } from "./latency";

const MIN_WIDTH_MS = 200;
/** A late outcome may still revise a closed bucket this far back. */
export const REVISION_MS = 10_000;

export interface LatencyBucket {
  phase: Stage | "idle";
  /** Buckets with different ids belong to separate lines. */
  continuityId: number;
  /** Timeline ms; `endT` is exclusive. */
  startT: number;
  endT: number;
  medianRttMs: number | null;
  minRttMs: number | null;
  p95RttMs: number | null;
  maxRttMs: number | null;
  /** Counts resolved probes. A timeout is a probe, never an RTT. */
  pingCount: number;
  timeoutCount: number;
}

/** One width for every stage length; with a fixed probe spacing no bucket is empty by construction. */
export function bucketWidth(intervalMs: number | null): number {
  return intervalMs ? Math.ceil(MIN_WIDTH_MS / intervalMs) * intervalMs : MIN_WIDTH_MS;
}

interface Open {
  startT: number;
  endT: number;
  rtts: number[];
  pings: number;
  timeouts: number;
}

export class LatencyBuckets {
  #phase: LatencyBucket["phase"] = "idle";
  #continuity = 0;
  #width = MIN_WIDTH_MS;
  #origin = 0;
  #warmup = false;
  #pending: Open | null = null;
  #closed: Open[] = [];
  /** Holds closed buckets revised since the last report. They go out once per tick, not once per outcome. */
  #revised = new Set<Open>();

  /** `emit` receives each bucket that closes with probes, and each revision at the next `closeThrough` or `flush`. */
  constructor(readonly emit: (bucket: LatencyBucket, warmup: boolean) => void) {}

  reset(
    t: number,
    phase: Stage,
    continuity: number,
    intervalMs: number | null,
    warmup = false,
  ): void {
    this.#phase = phase;
    this.#warmup = warmup;
    this.#width = bucketWidth(intervalMs);
    this.#origin = t;
    this.restart(t, continuity);
  }

  /** A new line's buckets keep the stage's grid, so a series folds them on the same slot edges. */
  restart(t: number, continuity: number): void {
    this.#continuity = continuity;
    this.#pending = this.#open(this.#aligned(t));
    this.#closed = [];
    this.#revised.clear();
  }

  observe(t: number, rttMs: number, timedOut: boolean): void {
    this.#pending ??= this.#open(this.#aligned(t));
    this.#close(t);
    const pending = this.#pending!;
    const target =
      t >= pending.startT
        ? pending
        : this.#closed.findLast((each) => t >= each.startT && t < each.endT);
    if (!target) return;
    target.pings++;
    if (timedOut) target.timeouts++;
    else if (Number.isFinite(rttMs)) target.rtts.push(Math.max(0, rttMs));
    if (target !== pending) this.#revised.add(target);
  }

  /** Closes every bucket that ends by `t`, whether or not a later probe arrives, and reports revisions since. */
  closeThrough(t: number): void {
    this.#report();
    this.#close(t);
  }

  #report(): void {
    for (const bucket of this.#revised) this.emit(this.#summary(bucket, bucket.endT), this.#warmup);
    this.#revised.clear();
  }

  #close(t: number): void {
    while (this.#pending && t >= this.#pending.endT) {
      const pending = this.#pending;
      // A page suspended for hours skips the empty time, since only the revision horizon can still take late outcomes.
      const skipped = pending.pings
        ? 0
        : Math.floor((t - pending.startT - REVISION_MS) / this.#width);
      if (skipped > 0) {
        // Everything closed before the pause ends outside the horizon.
        this.#closed = [];
        this.#pending = this.#open(pending.startT + skipped * this.#width);
        continue;
      }
      if (pending.pings) this.emit(this.#summary(pending, pending.endT), this.#warmup);
      this.#closed.push(pending);
      while (this.#closed[0]!.endT < pending.endT - REVISION_MS) this.#closed.shift();
      this.#pending = this.#open(pending.endT);
    }
  }

  /** The open bucket closes at this time, and the run's timer wakes then. */
  get nextEnd(): number | null {
    return this.#pending?.endT ?? null;
  }

  flush(atT: number): void {
    this.#report();
    const pending = this.#pending;
    this.#pending = null;
    this.#closed = [];
    if (pending?.pings)
      this.emit(
        this.#summary(pending, Math.max(pending.startT, Math.min(pending.endT, atT))),
        this.#warmup,
      );
  }

  #aligned(t: number): number {
    return this.#origin + Math.floor((t - this.#origin) / this.#width) * this.#width;
  }

  #open(startT: number): Open {
    return { startT, endT: startT + this.#width, rtts: [], pings: 0, timeouts: 0 };
  }

  #summary(bucket: Open, endT: number): LatencyBucket {
    const rtts = bucket.rtts.sort((a, b) => a - b);
    const n = rtts.length;
    const at = (rank: number) => rtts[rank]!;
    return {
      phase: this.#phase,
      continuityId: this.#continuity,
      startT: bucket.startT,
      endT,
      medianRttMs: n ? midpointMedian(n, at) : null,
      minRttMs: n ? rtts[0]! : null,
      p95RttMs: n ? at(nearestRank(0.95, n)) : null,
      maxRttMs: n ? rtts[n - 1]! : null,
      pingCount: bucket.pings,
      timeoutCount: bucket.timeouts,
    };
  }
}
