import { expect, test } from "bun:test";
import { RateBuckets, stabilityPct } from "./stability";

test("an unlimited series merges bucket pairs past 4,096, keeping its rates and its marks", () => {
  const series = new RateBuckets();
  // A day at 1,000 B/s, alternating 900 and 1,100 every second.
  for (let second = 0; second < 86_400; second++) series.observe(second % 2 ? 1_100 : 900, 1_000);
  expect(series.rates.length).toBeLessThanOrEqual(4_096);
  expect(series.buckets).toBeGreaterThanOrEqual(86_400 * 4 - series.width);
  expect(series.rates.every((rate) => Math.abs(rate - 1_000) < 1e-6)).toBe(true);
  expect(stabilityPct(series.rates)).toBeCloseTo(100);
  series.truncate(series.buckets / 2);
  expect(series.buckets).toBe(series.rates.length * series.width);
});

test("a short series keeps every 250 ms bucket", () => {
  const series = new RateBuckets();
  for (let second = 0; second < 60; second++) series.observe(second % 2 ? 1_100 : 900, 1_000);
  expect([series.rates.length, series.width]).toEqual([240, 1]);
});
