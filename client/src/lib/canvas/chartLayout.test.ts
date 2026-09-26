import { expect, test } from "bun:test";
import { chartLayout } from "./chartLayout";

const viewport = {
  tMin: 0,
  tMax: 10_000,
  bytesPerSecMax: 1_000,
  rttMin: 0,
  rttMax: 100,
};

test("chart layout keeps coordinate functions finite before a plot is measured", () => {
  const layout = chartLayout(0, 0, viewport);
  expect(layout.width).toBe(1);
  expect(layout.height).toBe(1);
  expect(layout.plot).toEqual({ left: 0, right: 1, top: 0, bottom: 1 });
  expect(Number.isFinite(layout.x(5_000))).toBe(true);
  expect(Number.isFinite(layout.latencyY(50))).toBe(true);
});
