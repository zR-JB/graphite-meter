// A strip's pure rules: rate lanes binned to the plot's width, the monotone curve through the bins, and reply bars
// that join whole buckets to a bar's pitch.
import type { Direction } from "../../engine/types";
import type { LatencyBucket } from "../../engine/measure/buckets";
import type { TransferStage } from "../../engine/run/schedule";
import type { ThroughputSample } from "../../app/series";

/** Plot columns are this wide, at least eight of them. */
const BIN_PX = 4;
/** A reply bar's least pitch: buckets narrower than this join, so bars keep air between them. */
const REPLY_PX = 4;

export const columnsFor = (width: number) => Math.max(8, Math.floor(width / BIN_PX));

/** One lane's non-empty bins: centre time (timeline ms) and mean rate (bytes/s), in time order. Buffers are kept and
    grown, so rebinning a live series allocates nothing once the width settles. */
export class Lane {
  t = new Float64Array(0);
  v = new Float64Array(0);
  line = new Float64Array(0);
  n = 0;
  #sums = new Float64Array(0);
  #counts = new Uint32Array(0);
  #lines = new Float64Array(0);

  /** Rebins one stage's samples of one direction; samples before `start` stay out and those past the span pile into
      the last column. */
  fill(
    samples: readonly ThroughputSample[],
    stage: TransferStage,
    dir: Direction,
    start: number,
    span: number,
    columns: number,
  ): void {
    if (this.#sums.length !== columns) {
      this.#sums = new Float64Array(columns);
      this.#counts = new Uint32Array(columns);
      this.t = new Float64Array(columns);
      this.v = new Float64Array(columns);
      this.line = new Float64Array(columns);
      this.#lines = new Float64Array(columns);
    } else {
      this.#sums.fill(0);
      this.#counts.fill(0);
    }
    const length = span || 1;
    for (const sample of samples) {
      if (sample.stage !== stage || sample.dir !== dir || sample.t < start) continue;
      const column = Math.min(columns - 1, Math.floor(((sample.t - start) / length) * columns));
      // A recovery inside a bin keeps only its new line, never an average with what failed before it.
      if (this.#lines[column] !== sample.continuityId) {
        this.#sums[column] = this.#counts[column] = 0;
        this.#lines[column] = sample.continuityId;
      }
      this.#sums[column]! += sample.bytesPerSec;
      this.#counts[column]!++;
    }
    this.n = 0;
    for (let column = 0; column < columns; column++) {
      if (!this.#counts[column]) continue;
      this.t[this.n] = start + ((column + 0.5) / columns) * length;
      this.line[this.n] = this.#lines[column]!;
      this.v[this.n++] = this.#sums[column]! / this.#counts[column]!;
    }
  }
}

/** Tangents of the monotone cubic (Fritsch–Carlson, weighted harmonic mean) through `n` points, written to `out`:
    the curve passes every point and never overshoots between two of them. */
export function monotoneTangents(
  x: ArrayLike<number>,
  y: ArrayLike<number>,
  n: number,
  out: Float64Array,
  lines?: ArrayLike<number>,
): void {
  if (n < 2) return;
  const slope = (i: number) => {
    if (lines && lines[i] !== lines[i + 1]) return 0;
    const h = x[i + 1]! - x[i]!;
    return h > 0 ? (y[i + 1]! - y[i]!) / h : 0;
  };
  out[0] = slope(0);
  out[n - 1] = slope(n - 2);
  for (let i = 1; i < n - 1; i++) {
    if (lines && lines[i] !== lines[i - 1]) {
      out[i] = slope(i);
      continue;
    }
    if (lines && lines[i] !== lines[i + 1]) {
      out[i] = slope(i - 1);
      continue;
    }
    const before = slope(i - 1);
    const after = slope(i);
    if (before * after <= 0) {
      out[i] = 0;
      continue;
    }
    const h0 = x[i]! - x[i - 1]!;
    const h1 = x[i + 1]! - x[i]!;
    const w1 = 2 * h1 + h0;
    const w2 = h1 + 2 * h0;
    out[i] = (w1 + w2) / (w1 / before + w2 / after);
  }
}

/** One bar of a reply track: the fastest to the slowest reply of the buckets it joins, at their centre time. */
export interface ReplyBar {
  t: number;
  t0: number;
  t1: number;
  lo: number;
  hi: number;
}

/** A stage's reply buckets as bars. Buckets narrower than a bar's pitch join whole, on the grid of the series' first
    bucket, so no bucket is drawn twice and a live bar only grows as its buckets close; a bucket of timeouts has none. */
export function replyBars(
  buckets: readonly LatencyBucket[],
  phase: LatencyBucket["phase"],
  start: number,
  span: number,
  width: number,
): ReplyBar[] {
  const replies = buckets.filter((bucket) => bucket.phase === phase && bucket.medianRttMs !== null);
  const origin = replies[0]?.startT ?? start;
  let bucketMs = 0;
  for (const bucket of replies.slice(0, 3)) bucketMs = Math.max(bucketMs, bucket.endT - bucket.startT);
  const pitchMs = (REPLY_PX / Math.max(1, width)) * (span || 1);
  const groupMs = bucketMs ? Math.max(1, Math.ceil(pitchMs / bucketMs)) * bucketMs : pitchMs;
  const bars: ReplyBar[] = [];
  let group = Number.NaN;
  let continuity = Number.NaN;
  for (const bucket of replies) {
    if (bucket.endT < start || bucket.startT > start + span) continue;
    const median = bucket.medianRttMs!;
    const lo = bucket.minRttMs ?? median;
    const hi = bucket.maxRttMs ?? median;
    const at = Math.floor(((bucket.startT + bucket.endT) / 2 - origin) / groupMs);
    const bar = bars.at(-1);
    if (at !== group || continuity !== bucket.continuityId || !bar) {
      group = at;
      continuity = bucket.continuityId;
      bars.push({ t: 0, t0: bucket.startT, t1: bucket.endT, lo, hi });
      continue;
    }
    bar.t1 = Math.max(bar.t1, bucket.endT);
    bar.lo = Math.min(bar.lo, lo);
    bar.hi = Math.max(bar.hi, hi);
  }
  for (const bar of bars) bar.t = (bar.t0 + bar.t1) / 2;
  return bars;
}

/** The point nearest `t` among points in time order; the earlier wins a tie. */
export function nearestIndex(count: number, time: (i: number) => number, t: number): number {
  let best = -1;
  for (let i = 0; i < count; i++) if (best < 0 || Math.abs(time(i) - t) < Math.abs(time(best) - t)) best = i;
  return best;
}
