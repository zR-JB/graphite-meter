import { expect, test } from "bun:test";
import { canvasPixelRatio } from "./canvasResolution";

test("normal canvas resolution retains the existing 2x cap", () => {
  expect(canvasPixelRatio(1, 1)).toBe(1);
  expect(canvasPixelRatio(2, 1)).toBe(2);
  expect(canvasPixelRatio(3, 1)).toBe(2);
});

test("pinch zoom gains bounded half-step resolution headroom", () => {
  expect(canvasPixelRatio(1, 1.2)).toBe(1.5);
  expect(canvasPixelRatio(2, 1.1)).toBe(2.5);
  expect(canvasPixelRatio(2, 1.5)).toBe(3);
  expect(canvasPixelRatio(2, 2)).toBe(4);
  expect(canvasPixelRatio(2, 4)).toBe(4);
});
