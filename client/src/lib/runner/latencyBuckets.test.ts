import { expect, test } from "bun:test";
import {
  latencyJitterMs,
  LatencyPresentationBuckets,
  LATENCY_PRESENTATION_BUCKET_MS,
} from "./latencyBuckets";

test("phase-aligned buckets retain median tail and loss summaries", () => {
  const buckets = new LatencyPresentationBuckets();
  buckets.reset(1_000, "latency", false, 7);
  expect(buckets.observe(1_010, 10, false)).toEqual([]);
  expect(buckets.observe(1_040, 100, false)).toEqual([]);
  expect(buckets.observe(1_080, 0, true)).toEqual([]);
  const emitted = buckets.observe(1_200, 20, false);
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
  const buckets = new LatencyPresentationBuckets();
  buckets.reset(0, "latency", false, 1);
  buckets.observe(10, 20, false);

  expect(buckets.closeThrough(199)).toEqual([]);
  expect(buckets.closeThrough(200)).toHaveLength(1);
  expect(buckets.closeThrough(200)).toEqual([]);
});

test("bucket summaries preserve exact consecutive RTT jitter", () => {
  const buckets = new LatencyPresentationBuckets();
  buckets.reset(0, "latency", false, 1);
  const summary = buckets.closeThrough(0);
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
    summary.push(...buckets.observe(t, rtt, false));
  summary.push(...buckets.closeThrough(400));

  expect(latencyJitterMs(summary)).toBeCloseTo(270 / 7, 10);
});

test("partial flush is truthful and an all-loss bucket has no RTT", () => {
  const buckets = new LatencyPresentationBuckets();
  buckets.reset(0, "download", true, 2);
  buckets.observe(30, 0, true);
  buckets.observe(90, 0, true);
  expect(buckets.flush(100)).toMatchObject({
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
    const buckets = new LatencyPresentationBuckets();
    buckets.reset(0, "latency", false, 1);
    const emitted = groups.flatMap((group) =>
      group.flatMap((outcome) =>
        buckets.observe(outcome.t, outcome.rtt, outcome.lost),
      ),
    );
    const tail = buckets.flush(450);
    return tail ? [...emitted, tail] : emitted;
  };

  expect(collect(outcomes.map((outcome) => [outcome]))).toEqual(
    collect([outcomes.slice(0, 3), outcomes.slice(3)]),
  );
});
