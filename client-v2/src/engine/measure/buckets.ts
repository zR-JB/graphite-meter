// Latency presentation: one server's probe outcomes grouped into time buckets on the run's timeline. Results never
// read them; a late outcome may still revise the buckets of the last ten seconds.
import type { Stage } from "../run/schedule";

/** Points a presented series keeps. */
export const SERIES_LIMIT = 1_200;
const MIN_WIDTH_MS = 200;
const REVISION_MS = 10_000;

export interface LatencyBucket {
  /** The stage the probes belong to; "idle" between runs. */
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
  /** Resolved probes; a timeout is a probe, never an RTT. */
  pingCount: number;
  timeoutCount: number;
}

/** Wide enough for a stage of `spanMs` to fit the series; with a fixed probe spacing a whole number of probes, so
    no bucket is empty by construction. */
export function bucketWidth(spanMs: number, intervalMs: number | null): number {
  const base = Math.max(MIN_WIDTH_MS, Math.ceil(Math.max(0, spanMs) / SERIES_LIMIT / MIN_WIDTH_MS) * MIN_WIDTH_MS);
  return intervalMs ? Math.ceil(base / intervalMs) * intervalMs : base;
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
  #intervalMs: number | null = null;
  #pending: Open | null = null;
  /** Closed buckets a late outcome may still revise. */
  #closed: Open[] = [];

  /** `emit` receives every bucket that closes with probes in it, and every revision of one. */
  constructor(readonly emit: (bucket: LatencyBucket) => void) {}

  reset(t: number, phase: Stage, continuity: number, spanMs: number, intervalMs: number | null): void {
    this.#phase = phase;
    this.#intervalMs = intervalMs;
    this.#width = bucketWidth(spanMs, intervalMs);
    this.restart(t, continuity);
  }

  /** A new line from `t`, with the same stage and width. */
  restart(t: number, continuity: number): void {
    this.#continuity = continuity;
    this.#pending = this.#open(t);
    this.#closed = [];
  }

  /** A longer stage only widens buckets, so drawn ones never split. */
  widen(spanMs: number): void {
    this.#width = Math.max(this.#width, bucketWidth(spanMs, this.#intervalMs));
    if (this.#pending) this.#pending.endT = this.#pending.startT + this.#width;
  }

  observe(t: number, rttMs: number, timedOut: boolean): void {
    this.#pending ??= this.#open(t);
    this.closeThrough(t);
    const pending = this.#pending!;
    const target = t >= pending.startT ? pending : this.#closed.findLast((each) => t >= each.startT && t < each.endT);
    if (!target) return;
    target.pings++;
    if (timedOut) target.timeouts++;
    else if (Number.isFinite(rttMs)) target.rtts.push(Math.max(0, rttMs));
    if (target !== pending) this.emit(this.#summary(target, target.endT));
  }

  /** Closes every bucket that ends by `t`, whether or not a later probe arrives. */
  closeThrough(t: number): void {
    while (this.#pending && t >= this.#pending.endT) {
      const pending = this.#pending;
      // A page suspended for hours skips the empty time: only the revision horizon can still take late outcomes.
      const skipped = pending.pings ? 0 : Math.floor((t - pending.startT - REVISION_MS) / this.#width);
      if (skipped > 0) {
        // Everything closed before the pause ends outside the horizon.
        this.#closed = [];
        this.#pending = this.#open(pending.startT + skipped * this.#width);
        continue;
      }
      if (pending.pings) this.emit(this.#summary(pending, pending.endT));
      this.#closed.push(pending);
      while (this.#closed[0]!.endT < pending.endT - REVISION_MS) this.#closed.shift();
      this.#pending = this.#open(pending.endT);
    }
  }

  /** When the open bucket closes; the run's timer wakes then. */
  get nextEnd(): number | null {
    return this.#pending?.endT ?? null;
  }

  /** Emits the open bucket cut at `atT` and forgets everything. */
  flush(atT: number): void {
    const pending = this.#pending;
    this.#pending = null;
    this.#closed = [];
    if (pending?.pings) this.emit(this.#summary(pending, Math.max(pending.startT, Math.min(pending.endT, atT))));
  }

  #open(startT: number): Open {
    return { startT, endT: startT + this.#width, rtts: [], pings: 0, timeouts: 0 };
  }

  #summary(bucket: Open, endT: number): LatencyBucket {
    const rtts = bucket.rtts.sort((a, b) => a - b);
    const n = rtts.length;
    return {
      phase: this.#phase,
      continuityId: this.#continuity,
      startT: bucket.startT,
      endT,
      medianRttMs: n ? (n % 2 ? rtts[n >> 1]! : (rtts[(n >> 1) - 1]! + rtts[n >> 1]!) / 2) : null,
      minRttMs: n ? rtts[0]! : null,
      p95RttMs: n ? rtts[Math.min(n - 1, Math.max(0, Math.ceil(0.95 * n) - 1))]! : null,
      maxRttMs: n ? rtts[n - 1]! : null,
      pingCount: bucket.pings,
      timeoutCount: bucket.timeouts,
    };
  }
}
