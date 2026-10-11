import { expect, spyOn, test } from "bun:test";
import { nextFrame } from "./frame";
import { Glide, Handoff, spring } from "./motion";
import { effect } from "./signal";

test("a sample glides in over the interval between samples, from where the value is", () => {
  const value = new Glide();
  value.set(100, { snap: true, now: 0 });
  value.set(200, { now: 250 });
  expect(value.at(250)).toBe(100);
  expect(value.at(375)).toBeCloseTo(150, 5);
  value.set(0, { now: 400 });
  expect(value.at(400)).toBeCloseTo(value.at(399.999), 2);
  expect(value.at(10_000)).toBe(0);
  value.dispose();
});

test("a clock keeps moving between samples, stops at its limit and absorbs corrections at its own pace", () => {
  const clock = new Glide();
  clock.set(0, { rate: 1, max: 1_000, snap: true, now: 0 });
  expect([clock.at(400), clock.at(5_000)]).toEqual([400, 1_000]);
  clock.set(450, { rate: 1, max: 1_000, now: 500 });
  expect([clock.at(500), clock.at(700), clock.at(900)]).toEqual([500, 675, 850]);
  clock.dispose();
});

test("a fixed glide overrides the interval, and finishing retargets within it before snapping", () => {
  const value = new Glide();
  value.set(800, { snap: true, now: 0 });
  value.set(0, { over: 800, now: 100 });
  expect([value.at(500), value.at(900)]).toEqual([400, 0]);
  const sweep = new Glide();
  sweep.set(0, { snap: true, now: 0 });
  sweep.set(100, { over: 400, ease: true, now: 0 });
  sweep.set(200, { finish: true, now: 100 });
  expect(sweep.at(300)).toBeGreaterThan(100);
  expect(sweep.at(400)).toBe(200);
  sweep.set(50, { finish: true, now: 600 });
  expect(sweep.at(600)).toBe(50);
  for (const glide of [value, sweep]) glide.dispose();
});

test("the first sample snaps and a sample that is not a finite number holds the value", () => {
  const value = new Glide();
  value.set(42, { now: 1_000 });
  for (const bad of [Number.NaN, Number.POSITIVE_INFINITY]) value.set(bad, { now: 1_100 });
  expect([value.at(1_100), value.current()]).toEqual([42, 42]);
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
    // Far enough on that every glide a test left moving lands, so no frame stays owed to the next test.
    nextFrame(() => {});
    frame(1e9);
    globalThis.requestAnimationFrame = raf;
    clock.mockRestore();
  }
}

test("a correction too small to show publishes nothing, and a glide still lands exactly", () =>
  withFrames((frame) => {
    const value = new Glide();
    value.set(20, { snap: true, now: 0 });
    value.set(20 + 1e-9, { now: 100 });
    frame(150);
    expect(value.current()).toBe(20);
    value.set(0, { over: 100, now: 200 });
    for (let now = 216; now <= 320; now += 16) frame(now);
    expect(value.current()).toBe(0);
  }));

test("at 120 Hz a live value redraws on every other frame and a fixed glide on every frame", () =>
  withFrames((frame) => {
    const [live, fixed] = [new Glide(), new Glide()];
    for (const glide of [live, fixed]) glide.set(0, { snap: true, now: 0 });
    live.set(1_000, { now: 400 });
    fixed.set(1_000, { over: 400, now: 400 });
    const seen = { live: new Set<number>(), fixed: new Set<number>() };
    for (let i = 1; i <= 60; i++) {
      frame(400 + i * (1_000 / 120));
      seen.live.add(live.current());
      seen.fixed.add(fixed.current());
    }
    expect([seen.fixed.size, seen.live.size]).toEqual([48, 25]);
    expect([live.current(), fixed.current()]).toEqual([1_000, 1_000]);
  }));

test("a hand-off never shows a key held shorter than its fade-out and follows a held key live", () =>
  withFrames((frame) => {
    let now = 0;
    const tick = () => frame((now += 16));
    const view = new Handoff({ phase: "latency", ms: 1 }, (v) => v.phase);
    const shown = new Set<string>();
    view.set({ phase: "warmup", ms: 1 });
    tick();
    view.set({ phase: "download", ms: 1 });
    for (let i = 0; i < 30; i++) {
      tick();
      shown.add(view.shown().phase);
    }
    expect([...shown]).toEqual(["latency", "download"]);
    expect(view.out()).toBe(false);
    view.set({ phase: "download", ms: 2 });
    expect(view.shown().ms).toBe(2);
  }));

test("disposing stops a glide or a hand-off where it is, and a new sample resumes", () =>
  withFrames((frame) => {
    const value = new Glide();
    value.set(0, { now: 0 });
    value.set(100, { now: 100 });
    frame(150);
    expect(value.current()).toBe(50);
    value.dispose();
    frame(250);
    expect(value.current()).toBe(50);
    value.set(200, { now: 300 });
    frame(600);
    expect(value.current()).toBe(200);
    const view = new Handoff("old", (v) => v);
    view.set("new");
    view.dispose();
    frame(1_000);
    expect([view.shown(), view.out()]).toEqual(["old", true]);
  }));

test("holding or snapping ends any glide where it is, with no speed left over", () => {
  const eased = new Glide();
  eased.set(0, { snap: true, now: 0 });
  eased.set(100, { over: 400, ease: true, now: 0 });
  eased.hold(200);
  const held = eased.at(200);
  expect([eased.at(250), eased.at(1_000)]).toEqual([held, held]);
  const fixed = new Glide();
  fixed.set(0, { snap: true, now: 0 });
  fixed.set(800, { over: 800, now: 0 });
  fixed.hold(100);
  fixed.set(500, { finish: true, now: 150 });
  expect(fixed.at(150)).toBe(500);
  for (const glide of [eased, fixed]) glide.dispose();
});

test("a clock never passes its bound, whether a correction or a sample would carry it past", () => {
  const clock = new Glide();
  clock.set(1_000, { rate: 1, max: 1_000, snap: true, now: 0 });
  clock.set(990, { rate: 1, max: 1_000, now: 100 });
  for (const t of [100, 110, 150, 1_000]) expect(clock.at(t)).toBeLessThanOrEqual(1_000);
  clock.set(1_050, { rate: 1, max: 1_000, now: 200 });
  expect(clock.at(200)).toBe(1_000);
  clock.dispose();
});

test("a sample set in reply to a drawn value keeps the glide moving", () =>
  withFrames((frame) => {
    const value = new Glide();
    value.set(0, { snap: true, now: 0 });
    value.set(100, { over: 100, now: 0 });
    const stop = effect(() => {
      if (value.current() === 100) value.set(200);
    });
    for (const t of [16, 100, 300, 600]) frame(t);
    expect(value.current()).toBe(200);
    stop();
    value.dispose();
  }));
