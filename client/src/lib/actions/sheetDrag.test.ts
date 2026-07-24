import { expect, test } from "bun:test";
import { sheetGestureIntent, shouldDismissSheet } from "./sheetDrag";

const dismisses = (distance: number, velocity = 0, releasedAfterMs = 0) =>
  shouldDismissSheet({ distance, height: 600, velocity, releasedAfterMs });

test("shouldDismissSheet: a small movement snaps back open", () => {
  expect(dismisses(40)).toBe(false);
  expect(dismisses(95, 2)).toBe(false);
});

test("shouldDismissSheet: a deliberate pull dismisses the sheet", () => {
  expect(dismisses(159)).toBe(false);
  expect(dismisses(160)).toBe(true);
});

test("shouldDismissSheet: a short sheet gives up before the fixed 160px", () => {
  const pull = (distance: number) =>
    shouldDismissSheet({
      distance,
      height: 200,
      velocity: 0,
      releasedAfterMs: 0,
    });
  expect(pull(50)).toBe(false);
  expect(pull(60)).toBe(true);
});

test("shouldDismissSheet: a recent flick dismisses after minimum travel", () => {
  expect(dismisses(96, 0.85, 80)).toBe(true);
  expect(dismisses(96, 0.84, 80)).toBe(false);
});

test("shouldDismissSheet: stale flick velocity does not dismiss after a pause", () => {
  expect(dismisses(80, 1.2, 81)).toBe(false);
});

test("sheetGestureIntent: waits for a deliberate vertical drag", () => {
  expect(sheetGestureIntent(2, 9, 0)).toBe("pending");
  expect(sheetGestureIntent(2, 10, 0)).toBe("drag");
  expect(sheetGestureIntent(12, 10, 0)).toBe("scroll");
});

test("sheetGestureIntent: drawer content scroll wins until its top edge", () => {
  expect(sheetGestureIntent(0, -20, 0)).toBe("scroll");
  expect(sheetGestureIntent(0, 20, 1)).toBe("scroll");
  expect(sheetGestureIntent(0, 20, 0)).toBe("drag");
});
