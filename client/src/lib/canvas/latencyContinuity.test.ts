import { expect, test } from "bun:test";
import type { LatencyBucket } from "../runner/contract";
import {
  latencyBucketsContinuous,
  materiallyDifferentP95,
  nearestLatencyBucketInContinuity,
} from "./latencyContinuity";

function bucket(
  t: number,
  overrides: Partial<LatencyBucket> = {},
): LatencyBucket {
  return {
    t,
    startT: t - 50,
    endT: t + 50,
    medianRttMs: 20,
    p95RttMs: 21,
    maxRttMs: 22,
    firstRttMs: 20,
    lastRttMs: 20,
    rttDeltaSumMs: 0,
    rttDeltaCount: 0,
    pingCount: 4,
    lossCount: 0,
    underLoad: false,
    phase: "latency",
    continuityId: 1,
    ...overrides,
  };
}

test("partial loss retains latency continuity while all-loss breaks it", () => {
  const first = bucket(100);
  const partialLoss = bucket(300, { lossCount: 1 });
  const allLoss = bucket(500, {
    medianRttMs: null,
    p95RttMs: null,
    maxRttMs: null,
    firstRttMs: null,
    lastRttMs: null,
    pingCount: 4,
    lossCount: 4,
  });

  expect(latencyBucketsContinuous(first, partialLoss)).toBe(true);
  expect(latencyBucketsContinuous(partialLoss, allLoss)).toBe(false);
});

test("nearest latency hover stays within a rendered continuity segment", () => {
  const first = bucket(100);
  const second = bucket(300);
  const afterGap = bucket(1_300);
  const buckets = [first, second, afterGap];

  expect(nearestLatencyBucketInContinuity(buckets, 230)).toBe(second);
  expect(nearestLatencyBucketInContinuity(buckets, 700)).toBeNull();
  expect(nearestLatencyBucketInContinuity(buckets, 1_300)).toBe(afterGap);
});

test("all-loss latency buckets remain hoverable without a fabricated RTT", () => {
  const lossOnly = bucket(500, {
    medianRttMs: null,
    p95RttMs: null,
    maxRttMs: null,
    firstRttMs: null,
    lastRttMs: null,
    pingCount: 4,
    lossCount: 4,
  });

  expect(nearestLatencyBucketInContinuity([lossOnly], 500)).toBe(lossOnly);
});

test("P95 presentation has one deterministic materiality threshold", () => {
  expect(materiallyDifferentP95(20, 22)).toBe(false);
  expect(materiallyDifferentP95(20, 23)).toBe(true);
  expect(materiallyDifferentP95(100, 114)).toBe(false);
  expect(materiallyDifferentP95(100, 115)).toBe(true);
});
