import { expect, test } from "bun:test";
import { gaugeLayout } from "./gaugeLayout";

const layout = gaugeLayout(480, 260);

test("label anchors sit on exact tick rays at one fixed radial clearance", () => {
  for (const [index, point] of layout.labelPoints.entries()) {
    const tick = layout.majorTicks[index * 2]!;
    const tickRadius = Math.hypot(
      tick.to.x - layout.center.x,
      tick.to.y - layout.center.y,
    );
    const labelRadius = Math.hypot(
      point.x - layout.center.x,
      point.y - layout.center.y,
    );
    expect(point.angle).toBe(tick.angle);
    expect(labelRadius - tickRadius).toBeCloseTo(8, 8);
    expect(
      (point.x - layout.center.x) * Math.sin(point.angle) -
        (point.y - layout.center.y) * Math.cos(point.angle),
    ).toBeCloseTo(0, 8);
  }
  expect(
    layout.labelPoints.map((point) => [point.anchorX, point.anchorY]),
  ).toEqual([
    ["end", "start"],
    ["end", "end"],
    ["center", "end"],
    ["start", "end"],
    ["start", "start"],
  ]);
});

test("gauge layout has a finite fallback before its container is measured", () => {
  const fallback = gaugeLayout(0, 0);
  expect(fallback.width).toBe(1);
  expect(fallback.height).toBe(1);
  expect(fallback.majorTicks).toHaveLength(9);
  expect(fallback.labelPoints).toHaveLength(5);
});
