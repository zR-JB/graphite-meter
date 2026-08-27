import { expect, test } from "bun:test";
import { progressWindow } from "./progressWindow";

test("batches bytes until the reporting cadence", () => {
  const progress = progressWindow(100);

  expect(progress.add(10, 149)).toBeNull();
  expect(progress.add(20, 150)).toEqual({ bytes: 30, elapsedMs: 50 });
});

test("flush returns the final partial window once", () => {
  const progress = progressWindow(100);

  expect(progress.add(17, 120)).toBeNull();
  expect(progress.flush(125)).toEqual({ bytes: 17, elapsedMs: 25 });
  expect(progress.flush(130)).toBeNull();
});

// The consumer divides bytes by elapsed time, so a window closed at the clock reading it opened at has no denominator.
test("a window with bytes but no elapsed time is not a measurement", () => {
  const progress = progressWindow(100);

  expect(progress.add(30, 150)).toEqual({ bytes: 30, elapsedMs: 50 });
  expect(progress.add(10, 150)).toBeNull();
  expect(progress.flush(150)).toBeNull();
});

test("reset discards bytes from the preceding measurement sequence", () => {
  const progress = progressWindow(100);

  expect(progress.add(17, 120)).toBeNull();
  progress.reset(200);
  expect(progress.flush(250)).toBeNull();
});
