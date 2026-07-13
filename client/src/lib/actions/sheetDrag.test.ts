import { expect, test } from "bun:test";
import { shouldDismissSheet } from "./sheetDrag";

const snap = (distance: number, velocity = 0, releasedAfterMs = 0) =>
  shouldDismissSheet({ distance, height: 600, velocity, releasedAfterMs });

test("small sheet movement snaps back open", () => {
  expect(snap(40)).toBe(false);
  expect(snap(47, 2)).toBe(false);
});

test("a deliberate pull dismisses the sheet", () => {
  expect(snap(159)).toBe(false);
  expect(snap(160)).toBe(true);
});

test("a recent flick dismisses after minimum travel", () => {
  expect(snap(48, 0.75, 80)).toBe(true);
  expect(snap(48, 0.74, 80)).toBe(false);
});

test("stale flick velocity does not dismiss after a pause", () => {
  expect(snap(80, 1.2, 81)).toBe(false);
});
