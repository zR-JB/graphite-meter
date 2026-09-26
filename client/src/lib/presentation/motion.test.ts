import "../state/runes.testutil";
import { expect, spyOn, test } from "bun:test";

const { Handoff, Smoothed } = await import("./motion.svelte");

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

test("the first sample and non-finite samples never make the value non-finite", () => {
  const value = new Smoothed();
  value.set(42, { now: 1_000 });
  expect(value.at(1_000)).toBe(42);
  for (const bad of [NaN, Infinity, undefined as unknown as number])
    value.set(bad, { now: 1_100 });
  expect(value.at(1_100)).toBe(42);
  expect(value.current).toBe(42);
});

test("a morph keeps its pace while samples retarget it, and snaps once landed", () => {
  const sweep = new Smoothed();
  sweep.set(0, { snap: true, now: 0 });
  sweep.set(100, { over: 400, now: 0 });
  sweep.set(200, { finish: true, now: 100 });
  expect(sweep.at(300)).toBeGreaterThan(100);
  expect(sweep.at(400)).toBe(200);
  sweep.set(50, { finish: true, now: 600 });
  expect(sweep.at(600)).toBe(50);
});

test("a handoff never shows a key shorter than its fade-out and follows a held key live", () => {
  const frames: FrameRequestCallback[] = [];
  const raf = globalThis.requestAnimationFrame;
  globalThis.requestAnimationFrame = (task) => frames.push(task);
  let clock = 0;
  const now = spyOn(performance, "now").mockImplementation(() => clock);
  const tick = (ms: number) => {
    clock += ms;
    for (const task of frames.splice(0)) task(clock);
  };
  const view = new Handoff({ phase: "latency", ms: 1 }, (v) => v.phase);
  const shown = new Set<string>();
  view.set({ phase: "warmup", ms: 1 });
  tick(16);
  view.set({ phase: "download", ms: 1 });
  for (let frame = 0; frame < 30; frame++) {
    tick(16);
    shown.add(view.shown.phase);
  }
  expect([...shown]).toEqual(["latency", "download"]);
  expect(view.opacity).toBe(1);
  view.set({ phase: "download", ms: 2 });
  expect(view.shown.ms).toBe(2);
  now.mockRestore();
  globalThis.requestAnimationFrame = raf;
});
