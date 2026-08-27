import { expect, test } from "bun:test";
import type { LatencyBucket } from "./contract";
import {
  latencyBucketExceedsScale,
  latencyScaleForHistory,
  LatencyScaleController,
  LATENCY_SCALE_SHRINK_DWELL_MS,
} from "./latencyScale";
function bucket(endT: number, median: number, max = median): LatencyBucket {
  return {
    t: endT - 100,
    startT: endT - 200,
    endT,
    medianRttMs: median,
    p95RttMs: median,
    maxRttMs: max,
    firstRttMs: median,
    lastRttMs: median,
    rttDeltaSumMs: 0,
    rttDeltaCount: 0,
    pingCount: 1,
    lossCount: 0,
    underLoad: false,
    phase: "latency",
    continuityId: 0,
  };
}
test("scale expands from robust medians and ignores one maximum spike", () => {
  const scale = new LatencyScaleController();
  expect(scale.observe(bucket(200, 25))).toBe(40);
  expect(scale.observe(bucket(400, 25, 2_000))).toBe(40);
  expect(scale.observe(bucket(600, 90))).toBe(200);
});
test("a one-ping median above the robust scale remains marked", () => {
  expect(latencyBucketExceedsScale(bucket(200, 100), 40)).toBe(true);
  expect(latencyBucketExceedsScale(bucket(200, 40), 40)).toBe(false);
});
test("scale shrinks only after a full dwell and one tier at a time", () => {
  const scale = new LatencyScaleController();
  scale.observe(bucket(200, 300));
  expect(scale.scaleMs).toBe(400);
  scale.reset();
  scale.observe(bucket(0, 150));
  expect(scale.scaleMs).toBe(200);
  scale.observe(bucket(6_200, 20));
  expect(scale.scaleMs).toBe(200);
  scale.observe(bucket(6_200 + LATENCY_SCALE_SHRINK_DWELL_MS, 20));
  expect(scale.scaleMs).toBe(100);
});
test("terminal history retains an older sustained latency domain", () => {
  const history = [
    ...Array.from({ length: 20 }, (_, index) => bucket((index + 1) * 200, 300)),
    ...Array.from({ length: 40 }, (_, index) => bucket((index + 21) * 200, 20)),
  ];
  const live = new LatencyScaleController();
  for (const sample of history) live.observe(sample);
  expect(live.scaleMs).toBeLessThan(300);
  expect(latencyScaleForHistory(history)).toBe(400);
});
test("terminal history keeps an isolated tail as a clipping marker", () => {
  const history = [
    ...Array.from({ length: 39 }, (_, index) => bucket((index + 1) * 200, 20)),
    bucket(8_000, 2_000),
  ];
  expect(latencyScaleForHistory(history)).toBe(40);
  expect(latencyBucketExceedsScale(history.at(-1)!, 40)).toBe(true);
});
