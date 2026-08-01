import { expect, test } from "bun:test";
import {
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
    pingCount: 3,
    lossCount: 1,
    continuityId: 7,
  });
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
