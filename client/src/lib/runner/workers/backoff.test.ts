import { test, expect } from "bun:test";
import { nextBackoff } from "./backoff";

test("nextBackoff: the first failure (prev 0) jumps straight to the minimum", () => {
  expect(nextBackoff(0, 100, 2000)).toBe(100);
});

test("nextBackoff: subsequent failures double", () => {
  expect(nextBackoff(100, 100, 2000)).toBe(200);
  expect(nextBackoff(200, 100, 2000)).toBe(400);
});

test("nextBackoff: clamps at the maximum", () => {
  expect(nextBackoff(1500, 100, 2000)).toBe(2000);
  expect(nextBackoff(2000, 100, 2000)).toBe(2000);
});
