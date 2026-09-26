import "../state/runes.testutil";
import { expect, test } from "bun:test";

const { Smoothed } = await import("./motion.svelte");

test("a sample glides in over the interval between samples, never stepping", () => {
  const value = new Smoothed();
  value.set(100, { snap: true, now: 0 });
  value.set(200, { now: 250 });
  expect(value.at(250)).toBe(100);
  expect(value.at(375)).toBeCloseTo(150, 5);
  value.set(0, { now: 400 });
  // The next glide starts where the value is, not at the last sample.
  expect(value.at(400)).toBeCloseTo(value.at(399.999), 2);
  expect(value.at(10_000)).toBe(0);
});

test("a clock keeps moving between samples, stops at its limit and absorbs corrections", () => {
  const clock = new Smoothed();
  clock.set(0, { rate: 1, max: 1_000, snap: true, now: 0 });
  expect(clock.at(400)).toBe(400);
  expect(clock.at(5_000)).toBe(1_000);
  clock.set(450, { rate: 1, max: 1_000, now: 500 });
  expect(clock.at(500)).toBe(500);
  // The correction is absorbed over one interval at the clock's own pace.
  expect(clock.at(900)).toBe(860);
  expect(clock.at(1_000)).toBe(950);
});

test("a fixed glide overrides the sample interval", () => {
  const value = new Smoothed();
  value.set(800, { snap: true, now: 0 });
  value.set(0, { over: 800, now: 100 });
  expect(value.at(500)).toBe(400);
  expect(value.at(900)).toBe(0);
});
