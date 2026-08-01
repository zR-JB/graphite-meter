import { expect, test } from "bun:test";
import type { LatencyBucket } from "./contract";
import {
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

test("scale shrinks only after a full dwell and one tier at a time", () => {
  const scale = new LatencyScaleController();
  scale.observe(bucket(200, 300));
  expect(scale.scaleMs).toBe(400);
  scale.reset();
  scale.observe(bucket(0, 150));
  expect(scale.scaleMs).toBe(200);
  // Move the robust window past the high value, then hold the lower target.
  scale.observe(bucket(6_200, 20));
  expect(scale.scaleMs).toBe(200);
  scale.observe(bucket(6_200 + LATENCY_SCALE_SHRINK_DWELL_MS, 20));
  expect(scale.scaleMs).toBe(100);
});
