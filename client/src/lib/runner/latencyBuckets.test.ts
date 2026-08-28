import { expect, test } from "bun:test";
import {
  latencyJitterMs,
  latencyPresentationBucketMs,
  LatencyPresentationBuckets,
  LATENCY_PRESENTATION_BUCKET_MS,
  singleLatencyBucket,
  upsertLatencyBucket,
} from "./latencyBuckets";
import type { LatencyBucket } from "./contract";
function buckets(
  startT = 0,
  phase: "latency" | "download" = "latency",
  underLoad = false,
  continuityId = 1,
  durationMs?: number,
  pingIntervalMs: number | null = null,
): LatencyPresentationBuckets {
  const result = new LatencyPresentationBuckets();
  result.reset(
    startT,
    phase,
    underLoad,
    continuityId,
    durationMs,
    pingIntervalMs,
  );
  return result;
}
test("long phases widen presentation buckets within the history budget", () => {
  expect(latencyPresentationBucketMs(4_000)).toBe(200);
  expect(latencyPresentationBucketMs(4_000_000)).toBe(3_400);
});
test("fixed ping cadences align presentation buckets to their wire interval", () => {
  expect(latencyPresentationBucketMs(4_000, 80)).toBe(240);
  expect(latencyPresentationBucketMs(4_000, 250)).toBe(250);
  expect(latencyPresentationBucketMs(4_000, 600)).toBe(600);
  expect(latencyPresentationBucketMs(4_000, null)).toBe(200);
  expect(latencyPresentationBucketMs(4_000_000, 80)).toBe(3_440);
});
test("live duration extensions widen the active latency bucket", () => {
  const result = buckets(0, "latency", false, 1, 4_000);
  expect(result.nextBoundaryT).toBe(200);
  result.widen(4_000_000);
  expect(result.nextBoundaryT).toBe(3_400);

  const aligned = buckets(0, "download", true, 1, 4_000, 250);
  aligned.widen(4_000_000);
  expect(aligned.nextBoundaryT).toBe(3_500);
  expect(aligned.nextBoundaryT! % 250).toBe(0);
});
test("ideal fixed-cadence outcomes produce evenly spaced occupied points", () => {
  for (const pingIntervalMs of [80, 250, 600]) {
    const result = buckets(0, "download", true, 1, 2_400, pingIntervalMs);
    const emitted: LatencyBucket[] = [];
    for (let t = 0; t < 2_400; t += pingIntervalMs)
      emitted.push(...result.observe(t, 20, false));
    const summary = result.flush(2_400);
    expect(summary).not.toBeNull();
    if (summary) emitted.push(summary);
    // Every emitted point is occupied; no empty bucket is manufactured.
    expect(emitted.every((bucket) => bucket.pingCount > 0)).toBe(true);
    const bucketMs = latencyPresentationBucketMs(2_400, pingIntervalMs);
    expect(emitted.map((bucket) => bucket.startT)).toEqual(
      emitted.map((_, index) => index * bucketMs),
    );
    for (let i = 1; i < emitted.length; i++)
      expect(emitted[i].startT - emitted[i - 1].startT).toBe(bucketMs);
  }
});
test("phase-aligned buckets retain median tail and loss summaries", () => {
  const result = buckets(1_000, "latency", false, 7);
  expect(result.observe(1_010, 10, false)).toEqual([]);
  expect(result.observe(1_040, 100, false)).toEqual([]);
  expect(result.observe(1_080, 0, true)).toEqual([]);
  const emitted = result.observe(1_200, 20, false);
  expect(emitted).toHaveLength(1);
  expect(emitted[0]).toMatchObject({
    startT: 1_000,
    endT: 1_000 + LATENCY_PRESENTATION_BUCKET_MS,
    medianRttMs: 55,
    p95RttMs: 100,
    maxRttMs: 100,
    firstRttMs: 10,
    lastRttMs: 100,
    rttDeltaSumMs: 90,
    rttDeltaCount: 1,
    pingCount: 3,
    lossCount: 1,
    continuityId: 7,
  });
});
test("closed windows emit without waiting for another observation", () => {
  const result = buckets();
  result.observe(10, 20, false);
  expect(result.closeThrough(199)).toEqual([]);
  expect(result.closeThrough(200)).toHaveLength(1);
  expect(result.closeThrough(200)).toEqual([]);
});
test("a late observation revises its original closed window", () => {
  const result = buckets();
  result.observe(50, 10, false);
  const history = result.closeThrough(200);
  const revised = result.observe(150, 100, false);
  expect(revised).toHaveLength(1);
  expect(revised[0]).toMatchObject({
    startT: 0,
    endT: 200,
    medianRttMs: 55,
    firstRttMs: 10,
    lastRttMs: 100,
    rttDeltaSumMs: 90,
    pingCount: 2,
  });
  upsertLatencyBucket(history, revised[0]);
  expect(history).toEqual(revised);
  expect(result.flush(400)).toBeNull();
});
test("late arrival order does not rewrite observation-time jitter", () => {
  const result = buckets();
  result.observe(150, 100, false);
  const history = result.closeThrough(200);
  const [revised] = result.observe(50, 10, false);
  upsertLatencyBucket(history, revised);
  expect(history[0]).toMatchObject({
    firstRttMs: 10,
    lastRttMs: 100,
    rttDeltaSumMs: 90,
    rttDeltaCount: 1,
  });
});
test("revised buckets replace rather than duplicate visible history", () => {
  const initial = buckets();
  initial.observe(50, 10, false);
  const history: LatencyBucket[] = initial.closeThrough(200);
  const [revised] = initial.observe(150, 20, false);
  upsertLatencyBucket(history, revised);
  expect(history).toHaveLength(1);
  expect(history[0].pingCount).toBe(2);
});
test("history mutations distinguish tail appends from required reindexing", () => {
  const history: LatencyBucket[] = [];
  const first = singleLatencyBucket(0, 10, false, "latency");
  const tail = singleLatencyBucket(400, 40, false, "latency");
  expect(upsertLatencyBucket(history, first)).toBe("tail-append");
  expect(upsertLatencyBucket(history, tail)).toBe("tail-append");
  expect(
    upsertLatencyBucket(
      history,
      { ...first, medianRttMs: 20 },
      Number.POSITIVE_INFINITY,
    ),
  ).toBe("structural-change");
  expect(
    upsertLatencyBucket(
      history,
      singleLatencyBucket(200, 30, false, "latency"),
      Number.POSITIVE_INFINITY,
    ),
  ).toBe("structural-change");
  expect(
    upsertLatencyBucket(
      history,
      singleLatencyBucket(600, 50, false, "latency"),
      3,
    ),
  ).toBe("structural-change");
});
test("bucket summaries preserve exact consecutive RTT jitter", () => {
  const result = buckets();
  const summary = result.closeThrough(0);
  for (const [t, rtt] of [
    [10, 10],
    [40, 10],
    [80, 10],
    [120, 100],
    [210, 10],
    [240, 10],
    [280, 10],
    [320, 100],
  ] as const)
    summary.push(...result.observe(t, rtt, false));
  summary.push(...result.closeThrough(400));
  expect(latencyJitterMs(summary)).toBeCloseTo(270 / 7, 10);
});
test("jitter skips losses but never invents variation across continuity", () => {
  const result = buckets();
  const summary = [
    ...result.observe(10, 10, false),
    ...result.closeThrough(200),
    ...result.observe(210, 0, true),
    ...result.closeThrough(400),
    ...result.observe(410, 20, false),
    ...result.closeThrough(600),
  ];
  expect(latencyJitterMs(summary)).toBe(10);
  result.reset(600, "download", true, 2);
  summary.push(
    ...result.observe(610, 200, false),
    ...result.observe(650, 220, false),
    ...result.closeThrough(800),
  );
  expect(latencyJitterMs(summary)).toBe(15);
});
test("partial flush is truthful and an all-loss bucket has no RTT", () => {
  const result = buckets(0, "download", true, 2);
  result.observe(30, 0, true);
  result.observe(90, 0, true);
  expect(result.flush(100)).toMatchObject({
    startT: 0,
    endT: 100,
    medianRttMs: null,
    p95RttMs: null,
    maxRttMs: null,
    pingCount: 2,
    lossCount: 2,
    underLoad: true,
  });
});
test("the same timed outcomes bucket identically regardless of callback grouping", () => {
  const outcomes = [
    { t: 20, rtt: 12, lost: false },
    { t: 75, rtt: 14, lost: false },
    { t: 180, rtt: 0, lost: true },
    { t: 210, rtt: 16, lost: false },
    { t: 390, rtt: 18, lost: false },
    { t: 410, rtt: 20, lost: false },
  ];
  const collect = (groups: (typeof outcomes)[]) => {
    const result = buckets();
    const emitted = groups.flatMap((group) =>
      group.flatMap((outcome) =>
        result.observe(outcome.t, outcome.rtt, outcome.lost),
      ),
    );
    const tail = result.flush(450);
    return tail ? [...emitted, tail] : emitted;
  };
  expect(collect(outcomes.map((outcome) => [outcome]))).toEqual(
    collect([outcomes.slice(0, 3), outcomes.slice(3)]),
  );
});
test("batched and out-of-order outcomes retain worker time buckets", () => {
  const result = buckets(0, "download", true, 1, 1_000, 250);
  result.observe(50, 10, false);
  const initial = result.closeThrough(250);
  const [revised] = result.observe(150, 100, false);
  expect(initial[0]).toMatchObject({ startT: 0, endT: 250 });
  expect(revised).toMatchObject({
    startT: 0,
    endT: 250,
    medianRttMs: 55,
    pingCount: 2,
  });
  expect(revised.t).toBe(125);
});
test("an absent cadence slot stays absent and a reported loss stays loss-only", () => {
  const result = buckets(0, "download", true, 1, 1_000, 250);
  const emitted: LatencyBucket[] = [];
  emitted.push(...result.observe(0, 10, false));
  emitted.push(...result.observe(250, 0, true));
  // There is intentionally no outcome in [500, 750); this is a real gap.
  emitted.push(...result.observe(750, 20, false));
  const tail = result.flush(1_000);
  if (tail) emitted.push(tail);

  expect(emitted.map((bucket) => bucket.startT)).toEqual([0, 250, 750]);
  expect(emitted.find((bucket) => bucket.startT === 500)).toBeUndefined();
  expect(emitted[1]).toMatchObject({
    pingCount: 1,
    lossCount: 1,
    medianRttMs: null,
  });
});
