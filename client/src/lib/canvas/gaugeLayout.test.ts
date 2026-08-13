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

test("gauge layout has a finite fallback before its container is measured", () => {
  const layout = gaugeLayout(0, 0, 0);
  expect(layout.width).toBe(1);
  expect(layout.height).toBe(1);
  expect(layout.labelPoints).toEqual([]);
});
