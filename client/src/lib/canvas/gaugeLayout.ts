const ARC_START = Math.PI * 0.75;
const ARC_SWEEP = Math.PI * 1.5;
export const GAUGE_TICK_FRACTIONS = [
  0,
  1 / 8,
  2 / 8,
  3 / 8,
  4 / 8,
  5 / 8,
  6 / 8,
  7 / 8,
  1,
] as const;
export const GAUGE_LABEL_FRACTIONS = [0, 2 / 8, 4 / 8, 6 / 8, 1] as const;
const LABEL_CLEARANCE = 8;
const AXIS_EPSILON = 1e-6;
interface GaugePoint {
  x: number;
  y: number;
}
interface GaugeLabelLayout extends GaugePoint {
  angle: number;
  anchorX: "start" | "center" | "end";
  anchorY: "start" | "center" | "end";
}
export interface GaugeLayout {
  width: number;
  height: number;
  center: GaugePoint;
  radius: number;
  arcWidth: number;
  arcStart: number;
  arcSweep: number;
  majorTicks: ReadonlyArray<{
    angle: number;
    from: GaugePoint;
    to: GaugePoint;
  }>;
  labelPoints: ReadonlyArray<GaugeLabelLayout>;
}
/** One CSS-pixel geometry model for the gauge canvas and DOM tick labels. */
export function gaugeLayout(width: number, height: number): GaugeLayout {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const center = { x: safeWidth / 2, y: safeHeight / 2 };
  const minimum = Math.min(safeWidth, safeHeight);
  const radius = Math.max(
    36,
    Math.min(minimum * 0.37, (minimum / 2 - 20) / 1.145),
  );
  const arcWidth = Math.max(6, radius * 0.13);
  const tickInner = radius + arcWidth * 0.5 + 3;
  const tickOuter = tickInner + radius * 0.08;
  const pointAt = (angle: number, distance: number): GaugePoint => ({
    x: center.x + Math.cos(angle) * distance,
    y: center.y + Math.sin(angle) * distance,
  });
  const majorTicks = GAUGE_TICK_FRACTIONS.map((fraction) => {
    const angle = ARC_START + fraction * ARC_SWEEP;
    return {
      angle,
      from: pointAt(angle, tickInner),
      to: pointAt(angle, tickOuter),
    };
  });
  const labelPoints = GAUGE_LABEL_FRACTIONS.map(
    (fraction): GaugeLabelLayout => {
      const angle = ARC_START + fraction * ARC_SWEEP;
      const point = pointAt(angle, tickOuter + LABEL_CLEARANCE);
      const horizontal = Math.cos(angle);
      const vertical = Math.sin(angle);
      return {
        ...point,
        angle,
        // The point is the label's nearest edge/corner on the exact tick ray.
        anchorX:
          horizontal < -AXIS_EPSILON
            ? "end"
            : horizontal > AXIS_EPSILON
              ? "start"
              : "center",
        anchorY:
          vertical < -AXIS_EPSILON
            ? "end"
            : vertical > AXIS_EPSILON
              ? "start"
              : "center",
      };
    },
  );
  return {
    width: safeWidth,
    height: safeHeight,
    center,
    radius,
    arcWidth,
    arcStart: ARC_START,
    arcSweep: ARC_SWEEP,
    majorTicks,
    labelPoints,
  };
}
