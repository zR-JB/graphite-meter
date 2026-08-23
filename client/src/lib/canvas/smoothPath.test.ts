import { expect, test } from "bun:test";
import { monotoneCurve } from "./smoothPath";

test("monotone curve passes through every measured point", () => {
  const points = [
    { x: 0, y: 20 },
    { x: 10, y: 18 },
    { x: 20, y: 8 },
    { x: 30, y: 8 },
  ];
  expect(monotoneCurve(points).map((segment) => segment.end)).toEqual(
    points.slice(1),
  );
});

test("regime transitions stay inside adjacent measurements", () => {
  const points = [
    { x: 0, y: 20 },
    { x: 10, y: 20 },
    { x: 20, y: 8 },
    { x: 30, y: 8 },
  ];
  for (const [i, segment] of monotoneCurve(points).entries()) {
    const low = Math.min(points[i].y, segment.end.y);
    const high = Math.max(points[i].y, segment.end.y);
    expect(segment.control1.y).toBeGreaterThanOrEqual(low);
    expect(segment.control1.y).toBeLessThanOrEqual(high);
    expect(segment.control2.y).toBeGreaterThanOrEqual(low);
    expect(segment.control2.y).toBeLessThanOrEqual(high);
  }
});
