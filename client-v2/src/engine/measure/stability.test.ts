import { expect, test } from "bun:test";
import { RateBuckets, stabilityPct } from "./stability";

test("an unlimited series merges the rates it keeps past 4,096, never the spread its stability reads", () => {
  const series = new RateBuckets();
  // A day at 1,000 B/s, alternating 900 and 1,100 every second: 250 ms rates 10% either side of the mean.
  for (let second = 0; second < 86_400; second++) series.observe(second % 2 ? 1_100 : 900, 1_000);
  expect(series.rates.length).toBeLessThanOrEqual(4_096);
  expect(series.buckets).toBeGreaterThanOrEqual(86_400 * 4 - series.width);
  expect(series.rates.every((rate) => Math.abs(rate - 1_000) < 1e-6)).toBe(true);
  expect(series.stability()).toBeCloseTo(90, 6);
});

test("a window opened mid-series reads only its own rates, and a mark taken before restores the series", () => {
  const series = new RateBuckets();
  for (let second = 0; second < 600; second++) series.observe(1_000, 1_000);
  const mark = series.mark;
  const kept = series.buckets;
  series.openWindow();
  for (let second = 0; second < 600; second++) series.observe(second % 2 ? 1_100 : 900, 1_000);
  expect(series.stability(true)).toBeCloseTo(90, 6);
  series.truncate(kept, mark);
  expect(series.stability()).toBeCloseTo(100, 6);
  expect(series.buckets).toBe(series.rates.length * series.width);
});

test("an unmerged series' stability is the stability of its rates", () => {
  const series = new RateBuckets();
  const rates = [800, 1_000, 1_250, 900, 1_100, 950];
  for (const rate of rates) series.observe(rate / 4, 250);
  expect(series.stability()).toBeCloseTo(stabilityPct(rates), 9);
});

test("a short series keeps every 250 ms bucket", () => {
  const series = new RateBuckets();
  for (let second = 0; second < 60; second++) series.observe(second % 2 ? 1_100 : 900, 1_000);
  expect([series.rates.length, series.width]).toEqual([240, 1]);
});
