import { test, expect } from "bun:test";
import { median } from "./stats";
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
