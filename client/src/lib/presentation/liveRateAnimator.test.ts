import { expect, test } from "bun:test";
import {
  LIVE_RATE_DECAY_HORIZON_MS,
  LIVE_RATE_STALE_DELAY_MS,
  LiveRateAnimator,
} from "./liveRateAnimator";

const input = (
  overrides: Partial<Parameters<LiveRateAnimator["step"]>[0]> = {},
) => ({
  key: "transfer" as const,
  target: 1_000,
  revision: 1,
  context: "1:upload",
  active: true,
  ...overrides,
});

test("seeds a new live context and eases later target revisions", () => {
  const animator = new LiveRateAnimator();
  expect(animator.step(input(), 0, false)).toEqual({
    value: 1_000,
    active: false,
  });
  const frame = animator.step(input({ target: 2_000, revision: 2 }), 50, false);
  expect(frame.value).toBeGreaterThan(1_000);
  expect(frame.value).toBeLessThan(2_000);
  expect(frame.active).toBe(true);
});

test("same numeric target revisions refresh freshness before stale decay", () => {
  const animator = new LiveRateAnimator();
  animator.step(input(), 0, false);
  animator.step(input({ revision: 2 }), LIVE_RATE_STALE_DELAY_MS - 1, false);
  const frame = animator.step(
    input({ revision: 2 }),
    LIVE_RATE_STALE_DELAY_MS * 2 - 1,
    false,
  );
  expect(frame.value).toBe(1_000);
});

test("a stale target decays quadratically after its grace interval", () => {
  const animator = new LiveRateAnimator();
  animator.step(input(), 0, false);
  const halfway = animator.step(
    input(),
    LIVE_RATE_STALE_DELAY_MS + LIVE_RATE_DECAY_HORIZON_MS / 2,
    true,
  );
  expect(halfway.value).toBe(750);
  const expired = animator.step(
    input(),
    LIVE_RATE_STALE_DELAY_MS + LIVE_RATE_DECAY_HORIZON_MS,
    true,
  );
  expect(expired).toEqual({ value: 0, active: false });
});

test("inactive, reset, and reduced-motion paths cannot carry a prior rate", () => {
  const animator = new LiveRateAnimator();
  animator.step(input(), 0, false);
  expect(
    animator.step(input({ active: false, target: 400 }), 20, false),
  ).toEqual({
    value: 400,
    active: false,
  });
  expect(
    animator.step(input({ context: "2:upload", target: 800 }), 40, true),
  ).toEqual({
    value: 800,
    active: false,
  });
});
