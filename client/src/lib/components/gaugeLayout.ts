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
// The share of the labelled ring's height the centring shifts it down, and the room between tick ends and a note.
const DROP = (1 - Math.SQRT1_2) / 2;
const NOTE_GAP = 8;
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
  /** Where a note hung under the ring starts: just under the tick ends. */
  noteTop: number;
  majorTicks: ReadonlyArray<{
    angle: number;
    from: GaugePoint;
    to: GaugePoint;
  }>;
  labelPoints: ReadonlyArray<GaugeLabelLayout>;
}
/** One CSS-pixel geometry model for the gauge surface and DOM tick labels; `note` is a band hung under the ring. */
export function gaugeLayout(
  width: number,
  height: number,
  note = 0,
): GaugeLayout {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const radius = Math.max(
    36,
    Math.min(
      safeWidth * 0.37,
      (safeWidth / 2 - 25) / 1.145,
      ((safeHeight - 42) / (1 + Math.SQRT1_2) - 11) / 1.145,
      // The labelled ring stays centred and its note still fits under the tick ends.
      note
        ? ((safeHeight / 2 - DROP * LABEL_CLEARANCE - NOTE_GAP - note) /
            (DROP + Math.SQRT1_2) -
            3) /
            1.145
        : Infinity,
    ),
  );
  const arcWidth = Math.max(6, radius * 0.085);
  const tickInner = radius + arcWidth * 0.5 + 6;
  const tickOuter = tickInner + Math.max(5, radius * 0.05);
  // Center the visible 270-degree sweep, including its label clearance.
  const center = {
    x: safeWidth / 2,
    y: safeHeight / 2 + DROP * (tickOuter + LABEL_CLEARANCE),
  };
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
    noteTop: center.y + Math.SQRT1_2 * tickOuter + NOTE_GAP,
    arcWidth,
    arcStart: ARC_START,
    arcSweep: ARC_SWEEP,
    majorTicks,
    labelPoints,
  };
}
