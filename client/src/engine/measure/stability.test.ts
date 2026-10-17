import { expect, test } from "bun:test";
import { RateSeries, stabilityPct } from "./stability";

test("a day-long series reads the spread of every 250 ms rate and keeps none of them", () => {
  const series = new RateSeries();
  // A day at 1,000 B/s alternates 900 and 1,100 every second, so the 250 ms rates sit 10% either side of the mean.
  for (let second = 0; second < 86_400; second++) series.observe(second % 2 ? 1_100 : 900, 1_000);
  expect(series.stability()).toBeCloseTo(90, 6);
  expect(series.recent).toEqual([]);
});

test("a window opened mid-series reads only its own rates, and a mark taken before restores the series", () => {
  const series = new RateSeries();
  for (let second = 0; second < 600; second++) series.observe(1_000, 1_000);
  const mark = series.mark;
  series.openWindow();
  for (let second = 0; second < 600; second++) series.observe(second % 2 ? 1_100 : 900, 1_000);
  expect(series.stability(true)).toBeCloseTo(90, 6);
  series.restore(mark);
  expect(series.stability()).toBeCloseTo(100, 6);
});

test("a series' stability is the stability of its 250 ms rates, however the evidence arrives", () => {
  const series = new RateSeries(16);
  const rates = [800, 1_000, 1_250, 900, 1_100, 950];
  for (const rate of rates) {
    series.observe(rate / 40, 25);
    series.observe((rate * 9) / 40, 225);
  }
  expect(series.stability()).toBeCloseTo(stabilityPct(rates), 9);
  expect(series.recent.map((rate) => Math.round(rate))).toEqual(rates);
});

test("a series keeps only its latest rates", () => {
  const series = new RateSeries(16);
  for (let bucket = 0; bucket < 240; bucket++) series.observe(bucket, 250);
  expect(series.recent).toEqual(Array.from({ length: 16 }, (_, i) => (224 + i) * 4));
});
