import { expect, test } from "bun:test";
import { gaugeLayout } from "./gaugeLayout";

test("gauge layout keeps canvas arc and DOM tick anchors in one CSS-pixel model", () => {
  const layout = gaugeLayout(480, 260, 5);
  expect(layout.center).toEqual({ x: 240, y: 130 });
  expect(layout.majorTicks).toHaveLength(9);
  expect(layout.labelPoints).toHaveLength(5);
  expect(layout.labelPoints[0].x).toBeLessThan(layout.center.x);
  expect(layout.labelPoints.at(-1)!.x).toBeGreaterThan(layout.center.x);
  expect(layout.labelPoints[0]).toMatchObject({
    anchorX: "start",
    anchorY: "start",
  });
  expect(layout.labelPoints[2]).toMatchObject({
    anchorX: "center",
    anchorY: "end",
  });
  expect(layout.labelPoints.at(-1)).toMatchObject({
    anchorX: "end",
    anchorY: "start",
  });
});

test("flank labels keep more clearance than the aligned top label", () => {
  const layout = gaugeLayout(480, 260, 5);
  const distance = (point: { x: number; y: number }) =>
    Math.hypot(point.x - layout.center.x, point.y - layout.center.y);

  expect(distance(layout.labelPoints[1]!)).toBeGreaterThan(
    distance(layout.labelPoints[2]!) + 4,
  );
  expect(distance(layout.labelPoints[3]!)).toBeGreaterThan(
    distance(layout.labelPoints[2]!) + 4,
  );
});

test("gauge labels preserve symmetric optical radial tiers", () => {
  const layout = gaugeLayout(480, 260, 5);
  const distance = (point: { x: number; y: number }) =>
    Math.hypot(point.x - layout.center.x, point.y - layout.center.y);
  const radii = layout.labelPoints.map(distance);

  expect(radii[1]).toBeCloseTo(radii[3], 8);
  expect(radii[0]).toBeCloseTo(radii[4], 8);
  expect(radii[1]).toBeGreaterThan(radii[0]!);
  expect(radii[0]).toBeGreaterThan(radii[2]!);
  expect(layout.labelPoints[2]!.x).toBeCloseTo(layout.center.x, 8);
});

test("gauge layout has a finite fallback before its container is measured", () => {
  const layout = gaugeLayout(0, 0, 0);
  expect(layout.width).toBe(1);
  expect(layout.height).toBe(1);
  expect(layout.labelPoints).toEqual([]);
});
