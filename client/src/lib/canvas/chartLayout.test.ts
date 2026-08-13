import { expect, test } from "bun:test";
import { chartLayout } from "./chartLayout";

const viewport = {
  tMin: 0,
  tMax: 10_000,
  bytesPerSecMax: 1_000,
  rttMin: 0,
  rttMax: 100,
};

test("chart layout shares CSS-pixel plot geometry across paths and DOM anchors", () => {
  const layout = chartLayout(600, 240, viewport);
  expect(layout.plot).toEqual({ left: 46, right: 554, top: 12, bottom: 222 });
  expect(layout.phaseRailY).toBe(226);
  expect(layout.timeLabelY).toBe(239);
  expect(layout.timeLabelY).toBeGreaterThan(layout.phaseRailY + 3);
  expect(layout.x(0)).toBe(layout.plot.left);
  expect(layout.x(10_000)).toBe(layout.plot.right);
  expect(layout.throughputY(1_000)).toBe(layout.plot.top);
  expect(layout.latencyY(0)).toBe(layout.plot.bottom);
  expect(layout.timeMajorTicks.length).toBeGreaterThan(1);
});

test("chart layout keeps coordinate functions finite before a plot is measured", () => {
  const layout = chartLayout(0, 0, viewport);
  expect(layout.width).toBe(1);
  expect(layout.height).toBe(1);
  expect(layout.plot).toEqual({ left: 0, right: 1, top: 0, bottom: 1 });
  expect(Number.isFinite(layout.x(5_000))).toBe(true);
  expect(Number.isFinite(layout.latencyY(50))).toBe(true);
});
