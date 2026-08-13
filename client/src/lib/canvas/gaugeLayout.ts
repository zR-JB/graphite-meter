const ARC_START = Math.PI * 0.75;
const ARC_SWEEP = Math.PI * 1.5;
const MAJOR_TICK_COUNT = 9;

export interface GaugePoint {
  x: number;
  y: number;
}

export interface GaugeLabelLayout extends GaugePoint {
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
  majorTicks: ReadonlyArray<{ from: GaugePoint; to: GaugePoint }>;
  labelPoints: ReadonlyArray<GaugeLabelLayout>;
}

/** One CSS-pixel geometry model for the gauge canvas and DOM tick labels. */
export function gaugeLayout(
  width: number,
  height: number,
  labelCount: number,
): GaugeLayout {
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
  const majorTicks = Array.from({ length: MAJOR_TICK_COUNT }, (_, index) => {
    const angle = ARC_START + (index / (MAJOR_TICK_COUNT - 1)) * ARC_SWEEP;
    return { from: pointAt(angle, tickInner), to: pointAt(angle, tickOuter) };
  });
  const labelRadius = tickOuter + 7;
  const labelPoints = Array.from(
    { length: Math.max(0, labelCount) },
    (_, index): GaugeLabelLayout => {
      const fraction = labelCount > 1 ? index / (labelCount - 1) : 0.5;
      const angle = ARC_START + fraction * ARC_SWEEP;
      const point = pointAt(angle, labelRadius);
      const horizontal = Math.cos(angle);
      const vertical = Math.sin(angle);
      return {
        ...point,
        angle,
        // Labels on the outer flanks grow inward; labels above or below the
        // arc grow away from it. The threshold leaves diagonal labels centered
        // until their optical relationship is unambiguous.
        anchorX:
          horizontal < -0.35 ? "start" : horizontal > 0.35 ? "end" : "center",
        anchorY:
          vertical < -0.35 ? "end" : vertical > 0.35 ? "start" : "center",
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
