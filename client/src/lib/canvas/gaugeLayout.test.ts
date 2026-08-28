import { expect, test } from "bun:test";
import {
  GAUGE_LABEL_FRACTIONS,
  GAUGE_TICK_FRACTIONS,
  gaugeLayout,
} from "./gaugeLayout";

const layout = gaugeLayout(480, 260);

test("gauge has nine uniform canvas ticks and five every-other labels", () => {
  expect(layout.majorTicks).toHaveLength(9);
  expect(layout.labelPoints).toHaveLength(5);
  const tickDeltas = layout.majorTicks
    .slice(1)
    .map((tick, index) => tick.angle - layout.majorTicks[index]!.angle);
  expect(
    tickDeltas.every((delta) => Math.abs(delta - tickDeltas[0]!) < 1e-10),
  ).toBe(true);
  const labelDeltas = layout.labelPoints
    .slice(1)
    .map((point, index) => point.angle - layout.labelPoints[index]!.angle);
  expect(
    labelDeltas.every((delta) => Math.abs(delta - labelDeltas[0]!) < 1e-10),
  ).toBe(true);
  expect(GAUGE_TICK_FRACTIONS).toEqual([
    0,
    1 / 8,
    2 / 8,
    3 / 8,
    4 / 8,
    5 / 8,
    6 / 8,
    7 / 8,
    1,
  ]);
  expect(GAUGE_LABEL_FRACTIONS).toEqual([0, 2 / 8, 4 / 8, 6 / 8, 1]);
});

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
