import { expect, test } from "bun:test";
import { sheetGestureIntent, shouldDismissSheet } from "./sheetDrag";

const snap = (distance: number, velocity = 0, releasedAfterMs = 0) =>
  shouldDismissSheet({ distance, height: 600, velocity, releasedAfterMs });

test("small sheet movement snaps back open", () => {
  expect(snap(40)).toBe(false);
  expect(snap(95, 2)).toBe(false);
});

test("a deliberate pull dismisses the sheet", () => {
  expect(snap(159)).toBe(false);
  expect(snap(160)).toBe(true);
});

test("a recent flick dismisses after minimum travel", () => {
  expect(snap(96, 0.85, 80)).toBe(true);
  expect(snap(96, 0.84, 80)).toBe(false);
});

test("stale flick velocity does not dismiss after a pause", () => {
  expect(snap(80, 1.2, 81)).toBe(false);
});

test("all surfaces wait for deliberate vertical drag intent", () => {
  expect(sheetGestureIntent(2, 9, 0)).toBe("pending");
  expect(sheetGestureIntent(2, 10, 0)).toBe("drag");
  expect(sheetGestureIntent(12, 10, 0)).toBe("scroll");
});

test("drawer content scroll wins until its top edge", () => {
  expect(sheetGestureIntent(0, -20, 0)).toBe("scroll");
  expect(sheetGestureIntent(0, 20, 1)).toBe("scroll");
  expect(sheetGestureIntent(0, 20, 0)).toBe("drag");
});
