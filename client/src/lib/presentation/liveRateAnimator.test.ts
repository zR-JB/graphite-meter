import { expect, test } from "bun:test";
import { LiveRateAnimator } from "./liveRateAnimator";

const input = (transfer = 1_500, down = 1_000, up = 500) => ({
  values: { transfer, down, up },
  context: "1:bidirectional",
  active: true,
});

test("readouts interpolate selected targets together and park when settled", () => {
  const animator = new LiveRateAnimator();
  expect(animator.step(input(), 0, false)).toEqual({
    values: input().values,
    active: false,
  });
  const frame = animator.step(input(3_000, 2_000, 1_000), 50, false);
  expect(frame.values.transfer).toBeGreaterThan(1_500);
  expect(frame.values.transfer).toBeLessThan(3_000);
  expect(frame.values.transfer).toBeCloseTo(
    frame.values.down + frame.values.up,
  );
  expect(frame.active).toBe(true);
  expect(animator.step(input(0, 0, 0), 2_050, false)).toEqual({
    values: { transfer: 0, down: 0, up: 0 },
    active: false,
  });
});

test("elapsed display time cannot invent decay of an unchanged selected target", () => {
  const animator = new LiveRateAnimator();
  animator.step(input(), 0, false);
  expect(animator.step(input(), 10_000, false)).toEqual({
    values: input().values,
    active: false,
  });
});

test("new contexts, inactive measurements and reduced motion snap to their selected rates", () => {
  const animator = new LiveRateAnimator();
  animator.step(input(), 0, false);
  for (const [next, reduced] of [
    [{ ...input(400, 400, 0), active: false }, false],
    [{ ...input(800, 0, 800), context: "2:upload" }, false],
    [{ ...input(600, 0, 600), context: "2:upload" }, true],
  ] as const) {
    expect(animator.step(next, 20, reduced)).toEqual({
      values: next.values,
      active: false,
    });
  }
});
