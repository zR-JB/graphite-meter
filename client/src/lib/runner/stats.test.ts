import { test, expect } from "bun:test";
import { median } from "./stats";

test("median: odd-length list returns the middle value", () => {
  expect(median([3, 1, 2])).toBe(2);
});

test("median: even-length list averages the two middle values", () => {
  expect(median([4, 1, 3, 2])).toBe(2.5);
});

test("median: single-element list returns that element", () => {
  expect(median([7])).toBe(7);
});

test("median: an empty list is 0, so a caller with no samples reads 0", () => {
  expect(median([])).toBe(0);
});

test("median: does not mutate the input array", () => {
  const xs = [3, 1, 2];
  median(xs);
  expect(xs).toEqual([3, 1, 2]);
});
