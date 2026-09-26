import { expect, test } from "bun:test";
import type { LiveSample } from "../runner/contract";
import { LiveRateAnimator, STALL_FADE_MS } from "./liveRateAnimator";

const live = (overrides: Partial<LiveSample> = {}): LiveSample => ({
  t: 0,
  phase: "bidirectional",
  continuityId: 1,
  bytes: 0,
  down: 1_000,
  up: 500,
  bridgedUp: null,
  stalled: false,
  ...overrides,
});
const values = (down: number, up: number) => ({ down, up });

test("readouts ease toward the targets together and park when settled", () => {
  const animator = new LiveRateAnimator();
  expect(animator.step(live(), 1, 0, false)).toEqual({
    values: values(1_000, 500),
    active: false,
  });
  const frame = animator.step(live({ down: 2_000, up: 1_000 }), 1, 50, false);
  expect(frame.values!.down).toBeGreaterThan(1_000);
  expect(frame.values!.down).toBeLessThan(2_000);
  expect(frame.values!.up / frame.values!.down).toBeCloseTo(0.5);
  expect(frame.active).toBe(true);
  expect(animator.step(live({ down: 0, up: 0 }), 1, 2_050, false)).toEqual({
    values: values(0, 0),
    active: false,
  });
  // Elapsed display time cannot invent decay of an unchanged target.
  expect(animator.step(live({ down: 0, up: 0 }), 1, 9_000, false).active).toBe(
    false,
  );
});

test("a stage change holds the last rate until its first evidence, which snaps", () => {
  const animator = new LiveRateAnimator();
  animator.step(live({ phase: "download", up: null }), 1, 0, false);
  for (const gap of [null, live({ phase: "upload", down: null, up: null })])
    expect(animator.step(gap, 1, 100, false).values).toEqual(values(1_000, 0));
  const upload = live({ phase: "upload", down: null, up: 400, bridgedUp: 450 });
  expect(animator.step(upload, 1, 200, false)).toEqual({
    values: values(0, 450),
    active: false,
  });
  expect(animator.step(null, 2, 300, false).values).toBeNull();
});

test("a stall fades linearly to exactly zero, at once with reduced motion", () => {
  const animator = new LiveRateAnimator();
  animator.step(live(), 1, 0, false);
  const stalled = live({ down: 0, up: 0, stalled: true });
  animator.step(stalled, 1, 100, false);
  expect(animator.step(stalled, 1, 100 + STALL_FADE_MS / 2, false)).toEqual({
    values: values(500, 250),
    active: true,
  });
  expect(animator.step(stalled, 1, 100 + STALL_FADE_MS, false)).toEqual({
    values: values(0, 0),
    active: false,
  });
  const reduced = new LiveRateAnimator();
  reduced.step(live(), 1, 0, true);
  expect(reduced.step(stalled, 1, 10, true).values).toEqual(values(0, 0));
});
