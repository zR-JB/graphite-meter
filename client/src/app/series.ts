// A run's chart series, bounded by time-bin compaction that never hides a peak, a dip or a line's ends.
import type { Direction } from "../engine/types";
import { REVISION_MS, SERIES_LIMIT, type LatencyBucket } from "../engine/measure/buckets";
import type { TransferStage } from "../engine/run/schedule";

export interface ThroughputSample {
  t: number;
  bytesPerSec: number;
  /** Unique measured bytes of the run so far. */
  bytesCumulative: number;
  dir: Direction;
  stage: TransferStage;
  continuityId: number;
}

/** Between runs the idle monitor adds one bucket per probe. It keeps this many. */
export const IDLE_LIMIT = 60;

const bucketLine = (bucket: LatencyBucket) => `${bucket.phase}:${bucket.continuityId}`;
const bucketTime = (bucket: LatencyBucket) => (bucket.startT + bucket.endT) / 2;
const sampleLine = (sample: ThroughputSample) =>
  `${sample.stage}:${sample.dir}:${sample.continuityId}`;
const sampleTime = (sample: ThroughputSample) => sample.t;

export function idleBucket(t: number, rttMs: number, timedOut: boolean): LatencyBucket {
  const rtt = timedOut ? null : Math.max(0, rttMs);
  return {
    phase: "idle",
    continuityId: 0,
    startT: t,
    endT: t,
    medianRttMs: rtt,
    minRttMs: rtt,
    p95RttMs: rtt,
    maxRttMs: rtt,
    pingCount: 1,
    timeoutCount: timedOut ? 1 : 0,
  };
}

/** Inserts a bucket in start order, or replaces its earlier version on the same line. */
export function upsertLatency(
  history: LatencyBucket[],
  bucket: LatencyBucket,
  limit = SERIES_LIMIT,
): void {
  const line = bucketLine(bucket);
  let at = history.length;
  while (at > 0 && history[at - 1]!.startT > bucket.startT) at--;
  let same = at - 1;
  while (
    same >= 0 &&
    history[same]!.startT === bucket.startT &&
    bucketLine(history[same]!) !== line
  )
    same--;
  if (same >= 0 && history[same]!.startT === bucket.startT) history[same] = bucket;
  else history.splice(at, 0, bucket);
  if (history.length <= limit) return;
  // Buckets a late outcome may still revise stay whole, so a revision replaces its bucket instead of merging.
  const horizon = Math.max(...history.map((each) => each.endT)) - REVISION_MS;
  const open = history.filter((each) => each.endT >= horizon);
  const whole = open.length < limit ? open : [];
  const settled = whole.length ? history.filter((each) => each.endT < horizon) : history;
  const reduced = compact(settled, limit - whole.length, bucketLine, bucketTime, (bin) => [
    mergeLatency(bin),
  ]);
  history.splice(0, history.length, ...[...reduced, ...whole].sort((a, b) => a.startT - b.startT));
}

/** Appends a sample, replacing one of the same line at the same time. `spanMs` is the run's planned length. */
export function appendThroughput(
  history: ThroughputSample[],
  sample: ThroughputSample,
  spanMs: number,
): void {
  upsertSample(history, sample);
  if (history.length > SERIES_LIMIT) compactThroughput(history, spanMs);
}

/** Keeps each line's first, last and per-bin extremes. */
function compactThroughput(history: ThroughputSample[], spanMs: number): void {
  const unique: ThroughputSample[] = [];
  for (const sample of history) upsertSample(unique, sample);
  history.splice(
    0,
    history.length,
    ...compact(unique, SERIES_LIMIT, sampleLine, sampleTime, extremes, spanMs, 4),
  );
}

/** Only the equal-time tail can hold the same line's sample. */
function upsertSample(history: ThroughputSample[], sample: ThroughputSample): void {
  const line = sampleLine(sample);
  for (let i = history.length - 1; i >= 0 && history[i]!.t === sample.t; i--)
    if (sampleLine(history[i]!) === line) {
      history[i] = sample;
      return;
    }
  history.push(sample);
}

function extremes(bin: ThroughputSample[]): ThroughputSample[] {
  const low = bin.reduce((a, b) => (b.bytesPerSec < a.bytesPerSec ? b : a));
  const high = bin.reduce((a, b) => (b.bytesPerSec > a.bytesPerSec ? b : a));
  return [...new Set([bin[0]!, low, high, bin.at(-1)!])];
}

/** A merged bucket keeps the success-weighted median and the worst tail. */
function mergeLatency(bin: LatencyBucket[]): LatencyBucket {
  const replies = bin
    .filter((b) => b.medianRttMs !== null)
    .sort((a, b) => a.medianRttMs! - b.medianRttMs!);
  const successes = replies.reduce((sum, b) => sum + b.pingCount - b.timeoutCount, 0);
  let seen = 0;
  const middle = replies.find((b) => (seen += b.pingCount - b.timeoutCount) >= successes / 2);
  const known = (values: (number | null)[]) => values.filter((value) => value !== null);
  const extreme = (pick: (...values: number[]) => number, values: (number | null)[]) =>
    known(values).length ? pick(...known(values)) : null;
  return {
    ...bin[0]!,
    endT: bin.at(-1)!.endT,
    medianRttMs: middle?.medianRttMs ?? null,
    minRttMs: extreme(
      Math.min,
      bin.map((b) => b.minRttMs),
    ),
    p95RttMs: extreme(
      Math.max,
      bin.map((b) => b.p95RttMs),
    ),
    maxRttMs: extreme(
      Math.max,
      bin.map((b) => b.maxRttMs),
    ),
    pingCount: bin.reduce((sum, b) => sum + b.pingCount, 0),
    timeoutCount: bin.reduce((sum, b) => sum + b.timeoutCount, 0),
  };
}

/** Doubles the bin width per line until the reduction fits, then samples evenly as a last resort. */
function compact<T>(
  history: readonly T[],
  limit: number,
  line: (value: T) => string,
  time: (value: T) => number,
  merge: (bin: T[]) => T[],
  spanMs = 0,
  pointsPerBin = 1,
): T[] {
  if (history.length <= 1 || limit <= 0) return history.length > limit ? [] : [...history];
  const lines = new Set(history.map(line)).size;
  const span = Math.max(1, spanMs, time(history.at(-1)!) - time(history[0]!));
  let width = Math.max(1, span / Math.max(1, Math.floor(limit / (lines * pointsPerBin))));
  let reduced: T[];
  do {
    const bins = Map.groupBy(
      history,
      (value) => `${line(value)}:${Math.floor(time(value) / width)}`,
    );
    reduced = [...bins.values()].flatMap(merge).sort((a, b) => time(a) - time(b));
    width *= 2;
  } while (reduced.length > limit && width / 2 < span);
  if (reduced.length <= limit) return reduced;
  if (limit === 1) return [reduced.at(-1)!];
  return Array.from(
    { length: limit },
    (_, i) => reduced[Math.round((i * (reduced.length - 1)) / (limit - 1))]!,
  );
}
