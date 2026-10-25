import { expect, test } from "bun:test";
import { LatencyBuckets, type LatencyBucket } from "../engine/measure/buckets";
import { Lane, replyBars } from "../ui/measure/strip-model";
import {
  BucketSeries,
  IDLE_LIMIT,
  idleBucket,
  SERIES_LIMIT,
  ThroughputSeries,
  type ThroughputSample,
} from "./series";

const bucket = (
  startT: number,
  rtt: number,
  phase: LatencyBucket["phase"] = "latency",
): LatencyBucket => ({
  ...idleBucket(startT, rtt, false),
  phase,
  endT: startT + 200,
  minRttMs: rtt - 1,
  maxRttMs: rtt + 1,
});

let seed = 7;
const noise = () => (seed = (seed * 16_807) % 2_147_483_647) / 2_147_483_647;

test("a revision replaces its bucket and an earlier start slots into order", () => {
  const series = new BucketSeries();
  for (const [t, rtt] of [
    [0, 10],
    [10, 12],
    [10, 14],
    [5, 8],
  ] as const)
    series.upsert(idleBucket(t, rtt, false));
  expect(series.buckets.map((each) => each.medianRttMs)).toEqual([10, 8, 14]);
});

test("two hours of 200 ms buckets keep the budget and the newest ten seconds whole, and folds never move a drawn bar", () => {
  const series = new BucketSeries();
  const spanMs = 2 * 3_600_000;
  const bars = () =>
    replyBars(series.buckets, "latency", 0, spanMs, 1_200).map(({ lo, hi, t0, t1 }) => [
      lo,
      hi,
      t0,
      t1,
    ]);
  let before = bars();
  let pings = 0;
  for (let t = 0; t < spanMs; t += 200) {
    series.upsert(bucket(t, 10 + 20 * noise()));
    pings++;
    const after = bars();
    // Only the live bar changes: it grows by the new bucket, or a new bar starts.
    expect(after.slice(0, before.length - 1)).toEqual(before.slice(0, -1));
    before = after;
  }
  expect(series.buckets.length).toBeLessThanOrEqual(SERIES_LIMIT);
  expect(series.buckets.reduce((sum, each) => sum + each.pingCount, 0)).toBe(pings);
  const recent = series.buckets.filter((each) => each.endT > spanMs - 10_000);
  expect(recent.every((each) => each.endT - each.startT === 200)).toBe(true);
  expect(recent).toHaveLength(50);
});

test("a fold keeps the success-weighted median and the worst tail, and a later revision replaces its bucket", () => {
  const series = new BucketSeries(2);
  for (const [i, rtt] of [10, 90, 20, 40, 50].entries()) series.upsert(bucket(i * 200, rtt));
  series.upsert({
    ...bucket(1_000, 0),
    medianRttMs: null,
    minRttMs: null,
    maxRttMs: null,
    timeoutCount: 1,
  });
  series.upsert(bucket(20_000, 30));
  series.upsert(bucket(20_000, 35));
  expect(series.buckets).toHaveLength(2);
  expect(series.buckets[0]).toMatchObject({
    medianRttMs: 40,
    minRttMs: 9,
    maxRttMs: 91,
    pingCount: 6,
    timeoutCount: 1,
  });
  expect(series.buckets[1]).toMatchObject({ medianRttMs: 35, pingCount: 1 });
});

test("probes denser than the budget's horizon keep the history folded", () => {
  const series = new BucketSeries(IDLE_LIMIT);
  let longest = 0;
  for (let t = 0; t < 3_600_000; t += 100) {
    series.upsert(idleBucket(t, 10, false));
    longest = Math.max(longest, series.buckets.length);
  }
  // The ten-second horizon stays whole, past the budget; everything before it folds into a slot or two.
  expect(longest).toBeGreaterThan(IDLE_LIMIT);
  expect(longest).toBeLessThanOrEqual(10_000 / 100 + 3);
});

test("a reconnect and a cut-short first bucket move no reply bar away from the break", () => {
  const series = new BucketSeries();
  const presented = new LatencyBuckets((bucket) => series.upsert(bucket));
  const spanMs = 3_600_000;
  const breaks = [150, 1_234_550];
  const bars = () => replyBars(series.buckets, "latency", 0, spanMs, 1_200);
  presented.reset(0, "latency", 0, null);
  let before = bars();
  let moved = 0;
  for (let t = 0; t < spanMs; t += 50) {
    const at = breaks.indexOf(t);
    if (at >= 0) {
      presented.flush(t);
      presented.restart(t, at + 1);
    }
    presented.observe(t, 10 + 20 * noise(), false);
    presented.closeThrough(t);
    const after = bars();
    const near = (bar: { t0: number; t1: number }) =>
      breaks.some((point) => bar.t1 > point - 10_000 && bar.t0 < point + 10_000);
    const drawn = new Set(after.map((bar) => JSON.stringify(bar)));
    for (const bar of before.slice(0, -1))
      if (!near(bar) && !drawn.has(JSON.stringify(bar))) moved++;
    before = after;
  }
  expect(moved).toBe(0);
});

test("a short stage after a long one keeps its own detail", () => {
  const series = new BucketSeries();
  for (let t = 0; t < 3_600_000; t += 200) series.upsert(bucket(t, 10));
  for (let t = 3_600_000; t < 3_610_000; t += 250) series.upsert(bucket(t, 30, "download"));
  for (let t = 3_610_000; t < 3_640_000; t += 200) series.upsert(bucket(t, 10, "upload"));
  expect(series.buckets.filter((each) => each.phase === "download")).toHaveLength(40);
  expect(series.buckets.length).toBeLessThanOrEqual(SERIES_LIMIT);
});

/** Peak and Stability so far as the cards computed them before, over every sample the stage took. */
function exactShape(samples: readonly ThroughputSample[]) {
  const seconds = new Map<number, Map<string, { sum: number; n: number }>>();
  for (const sample of samples) {
    const second = Math.floor((sample.t - samples[0]!.t) / 1000);
    const lanes = seconds.get(second) ?? new Map<string, { sum: number; n: number }>();
    seconds.set(second, lanes);
    const cell = lanes.get(sample.dir) ?? { sum: 0, n: 0 };
    lanes.set(sample.dir, cell);
    cell.sum += sample.bytesPerSec;
    cell.n++;
  }
  const rates = [...seconds.values()].map((lanes) =>
    lanes.values().reduce((total, { sum, n }) => total + sum / n, 0),
  );
  const mean = rates.reduce((a, b) => a + b, 0) / rates.length;
  const cv =
    Math.sqrt(rates.reduce((sum, rate) => sum + (rate - mean) ** 2, 0) / rates.length) / mean;
  return { peak: Math.max(...rates), stability: Math.max(0, 1 - cv) * 100 };
}

test("three hours of bidirectional samples keep the budget, exact Peak and Stability so far, and steady strip columns", () => {
  const series = new ThroughputSeries();
  const all: ThroughputSample[] = [];
  const spanMs = 3 * 3_600_000;
  const columns = 150;
  const lane = new Lane();
  const means = () => {
    lane.fill(series.samples, "bidirectional", "down", 0, spanMs, columns);
    return Array.from(lane.v.subarray(0, lane.n));
  };
  let before = means();
  let worst = 0;
  for (let t = 0; t < spanMs; t += 60) {
    for (const dir of ["down", "up"] as const) {
      const sample: ThroughputSample = {
        t,
        // A slow swell under noise, so columns differ and a misplaced sample would show.
        bytesPerSec: 1e8 * (1 + 0.5 * Math.sin(t / 600_000)) * (0.8 + 0.4 * noise()),
        bytesCumulative: t,
        dir,
        stage: "bidirectional",
        continuityId: 0,
        ticks: 1,
        t0: t,
        t1: t,
      };
      all.push(sample);
      series.append(sample);
    }
    if (t % 6_000) continue;
    const after = means();
    for (let i = 0; i < before.length - 1; i++)
      worst = Math.max(worst, Math.abs(after[i]! - before[i]!) / before[i]!);
    before = after;
  }
  // Every column but the live one keeps its mean within half a percent as folds go by, under a pixel on the strip.
  expect(worst).toBeLessThan(0.005);
  expect(series.samples.length).toBeLessThanOrEqual(SERIES_LIMIT);
  expect(series.samples.reduce((sum, each) => sum + each.ticks, 0)).toBe(all.length);
  const trace = series.traces.bidirectional!;
  const shape = trace.shape()!;
  const exact = exactShape(all);
  expect(shape.peak).toBeCloseTo(exact.peak, 3);
  expect(shape.stability!).toBeCloseTo(exact.stability, 9);
  expect([trace.first, trace.last, trace.count]).toEqual([0, spanMs - 60, all.length]);
});

test("a fold never averages a recovery with what failed before it", () => {
  const series = new ThroughputSeries();
  const recovery = 1_000_020;
  for (let t = 0; t < 3_600_000; t += 60)
    series.append({
      t,
      t0: t,
      t1: t,
      bytesPerSec: t < recovery - 3_000 ? 1_000 : t < recovery ? 0 : 1_000,
      bytesCumulative: t,
      dir: "down",
      stage: "download",
      continuityId: +(t >= recovery),
      ticks: 1,
    });
  for (const sample of series.samples)
    expect(sample.continuityId ? sample.t0 >= recovery : sample.t1 < recovery).toBe(true);
});
