import { expect, test } from "bun:test";
import { FixedRateBuckets, PairedRateBuckets } from "./controlBuckets";
test("equivalent exact traces produce identical 250 ms buckets", () => {
  const feed = (cadences: number[]) => {
    const buckets = new FixedRateBuckets();
    for (const durationMs of cadences)
      buckets.observe((2_000 * durationMs) / 1_000, durationMs);
    return [...buckets.rates];
  };
  expect(feed(Array(50).fill(20))).toEqual(feed([60, 137, 3, 400, 400]));
  expect(feed(Array(50).fill(20))).toEqual([2_000, 2_000, 2_000, 2_000]);
});
test("bucket edges prorate one irregular observation without losing bytes", () => {
  const buckets = new FixedRateBuckets();
  buckets.observe(100, 100);
  buckets.observe(400, 400);
  expect([...buckets.rates]).toEqual([1_000, 1_000]);
});
test("paired lanes keep a shared capped bucket window", () => {
  const buckets = new PairedRateBuckets(16);
  for (let i = 0; i < 32; i++) buckets.observe("down", i * 0.25, 250);
  for (let i = 0; i < 24; i++) buckets.observe("up", (100 + i) * 0.25, 250);
  expect([...buckets.rates]).toEqual(
    Array.from({ length: 8 }, (_, i) => 16 + i + 116 + i),
  );
});
test("timestamped paired lanes retain only overlapping temporal buckets", () => {
  const buckets = new PairedRateBuckets(16);
  for (let i = 0; i < 40; i++) buckets.observe("down", 250, 250, (i + 1) * 250);
  for (let i = 0; i < 16; i++)
    buckets.observe("up", 500, 250, (24 + i + 1) * 250);
  expect(buckets.completedCount).toBe(16);
  expect([...buckets.rates]).toEqual(Array(16).fill(3_000));
  buckets.observe("up", 500, 250, 80 * 250 + 250);
  expect(buckets.completedCount).toBe(0);
  buckets.reset();
  buckets.observe("down", 250, 250, 250);
  buckets.observe("up", 500, 250, 250);
  expect([...buckets.rates]).toEqual([3_000]);
});
