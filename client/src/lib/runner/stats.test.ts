import { test, expect } from "bun:test";
import { median, weightedMean, weightedMeanAbsoluteDeviation } from "./stats";
test.each([
  ["median: odd-length list returns the middle value", [3, 1, 2], 2],
  [
    "median: even-length list averages the two middle values",
    [4, 1, 3, 2],
    2.5,
  ],
  ["median: single-element list returns that element", [7], 7],
  ["median: an empty list is 0, so a caller with no samples reads 0", [], 0],
])("%s", (_label, values, expected) => {
  expect(median(values)).toBe(expected);
});
test("median: does not mutate the input array", () => {
  const xs = [3, 1, 2];
  median(xs);
  expect(xs).toEqual([3, 1, 2]);
});
test("weighted latency deviation follows represented successful pings", () => {
  const values = [
    { value: 10, weight: 9 },
    { value: 100, weight: 1 },
  ];
  const center = weightedMean(values)!;
  expect(center).toBe(19);
  expect(weightedMeanAbsoluteDeviation(values, center)).toBeCloseTo(16.2, 10);
});
