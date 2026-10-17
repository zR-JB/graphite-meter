import { expect, spyOn, test } from "bun:test";
import { nextFrame } from "./frame";
import { Glide, handoff, spring } from "./motion";
import { scope, signal } from "./signal";

test("a sample glides in over the interval between samples, from where the value is", () => {
  const value = new Glide();
  value.set(100, { snap: true, now: 0 });
  value.set(200, { now: 250 });
  expect(value.at(250)).toBe(100);
  expect(value.at(375)).toBeCloseTo(150, 5);
  value.set(0, { now: 400 });
  expect(value.at(400)).toBeCloseTo(value.at(399.999), 2);
  expect(value.at(10_000)).toBe(0);
});

test("a clock keeps moving between samples, stops at its limit and absorbs corrections at its own pace", () => {
  const clock = new Glide();
  clock.set(0, { rate: 1, max: 1_000, snap: true, now: 0 });
  expect([clock.at(400), clock.at(5_000)]).toEqual([400, 1_000]);
  clock.set(450, { rate: 1, max: 1_000, now: 500 });
  expect([clock.at(500), clock.at(700), clock.at(900)]).toEqual([500, 675, 850]);
});

test("a fixed glide overrides the interval, and a sample during one lands within its time", () => {
  const value = new Glide();
  value.set(800, { snap: true, now: 0 });
  value.set(0, { over: 800, now: 100 });
  expect([value.at(500), value.at(900)]).toEqual([400, 0]);
  const sweep = new Glide();
  sweep.set(0, { snap: true, now: 0 });
  sweep.set(100, { over: 400, now: 0 });
  sweep.set(200, { now: 100 });
  expect(sweep.at(300)).toBeGreaterThan(100);
  expect(sweep.at(400)).toBe(200);
});

test("the first sample snaps and a sample that is not a finite number holds the value", () => {
  const value = new Glide();
  value.set(42, { now: 1_000 });
  for (const bad of [Number.NaN, Number.POSITIVE_INFINITY]) value.set(bad, { now: 1_100 });
  expect(value.at(1_100)).toBe(42);
});

test("the spring overshoots once by about 1.3 % and settles within 1 % from about 0.26 s", () => {
  expect(spring([1, 0], 223)[0]).toBeCloseTo(-0.013, 3);
  expect(Math.abs(spring([1, 0], 259)[0])).toBeLessThanOrEqual(0.0101);
  expect(Math.abs(spring([1, 0], 420)[0])).toBeLessThan(0.0002);
});

/** Drives the frame clock by hand, then flushes it so later tests start clean. */
function withFrames(body: (frame: (now: number) => void) => void): void {
  const owed: FrameRequestCallback[] = [];
  const raf = globalThis.requestAnimationFrame;
  globalThis.requestAnimationFrame = (task) => owed.push(task);
  let last = 0;
  const clock = spyOn(performance, "now").mockImplementation(() => last);
  const frame = (now: number) => {
    last = now;
    for (const task of owed.splice(0)) task(now);
  };
  try {
    body(frame);
  } finally {
    // Far enough on that every hand-off a test left fading lands, so no frame stays owed to the next test.
    nextFrame(() => {});
    frame(1e9);
    globalThis.requestAnimationFrame = raf;
    clock.mockRestore();
  }
}

test("a hand-off never shows a key held shorter than its fade-out and follows a held key live", () =>
  withFrames((frame) => {
    let now = 0;
    const tick = () => frame((now += 16));
    const value = signal({ phase: "latency", ms: 1 });
    const [view, dispose] = scope(() => handoff(value, (v) => v.phase));
    const shown = new Set<string>();
    value.set({ phase: "warmup", ms: 1 });
    tick();
    value.set({ phase: "download", ms: 1 });
    for (let i = 0; i < 30; i++) {
      tick();
      shown.add(view.shown().phase);
    }
    expect([...shown]).toEqual(["latency", "download"]);
    expect(view.out()).toBe(false);
    value.set({ phase: "download", ms: 2 });
    expect(view.shown().ms).toBe(2);
    dispose();
  }));

test("disposing stops a hand-off where it is", () =>
  withFrames((frame) => {
    const value = signal("old");
    const [view, dispose] = scope(() => handoff(value, (v) => v));
    value.set("new");
    dispose();
    frame(1_000);
    expect([view.shown(), view.out()]).toEqual(["old", true]);
  }));

test("holding ends a fixed glide where it is, and the next sample glides on from there", () => {
  const fixed = new Glide();
  fixed.set(0, { snap: true, now: 0 });
  fixed.set(800, { over: 800, now: 0 });
  fixed.hold(100);
  expect([fixed.at(100), fixed.at(1_000)]).toEqual([100, 100]);
  fixed.set(500, { now: 150 });
  expect([fixed.at(150), fixed.at(200)]).toEqual([100, 500]);
});

test("a clock never passes its bound, whether a correction or a sample would carry it past", () => {
  const clock = new Glide();
  clock.set(1_000, { rate: 1, max: 1_000, snap: true, now: 0 });
  clock.set(990, { rate: 1, max: 1_000, now: 100 });
  for (const t of [100, 110, 150, 1_000]) expect(clock.at(t)).toBeLessThanOrEqual(1_000);
  clock.set(1_050, { rate: 1, max: 1_000, now: 200 });
  expect(clock.at(200)).toBe(1_000);
});
