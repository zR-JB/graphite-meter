interface PathPoint {
  x: number;
  y: number;
}
interface CurveSegment {
  control1: PathPoint;
  control2: PathPoint;
  end: PathPoint;
}
export function monotoneCurve(
  points: ReadonlyArray<PathPoint>,
): CurveSegment[] {
  if (points.length < 2) return [];
  const spans: number[] = [];
  const slopes: number[] = [];
  for (let i = 0; i < points.length - 1; i++) {
    const span = points[i + 1].x - points[i].x;
    spans.push(span);
    slopes.push(span > 0 ? (points[i + 1].y - points[i].y) / span : 0);
  }
  const tangents = [slopes[0]];
  for (let i = 1; i < points.length - 1; i++) {
    const before = slopes[i - 1];
    const after = slopes[i];
    if (before * after <= 0) {
      tangents.push(0);
      continue;
    }
    const beforeWeight = 2 * spans[i] + spans[i - 1];
    const afterWeight = spans[i] + 2 * spans[i - 1];
    tangents.push(
      (beforeWeight + afterWeight) /
        (beforeWeight / before + afterWeight / after),
    );
  }
  tangents.push(slopes.at(-1)!);
  return points.slice(1).map((end, i) => {
    const start = points[i];
    const third = spans[i] / 3;
    return {
      control1: {
        x: start.x + third,
        y: start.y + tangents[i] * third,
      },
      control2: {
        x: end.x - third,
        y: end.y - tangents[i + 1] * third,
      },
      end,
    };
  });
}
export function traceSmoothLine(
  ctx: CanvasRenderingContext2D,
  points: ReadonlyArray<PathPoint>,
): void {
  for (const segment of monotoneCurve(points))
    ctx.bezierCurveTo(
      segment.control1.x,
      segment.control1.y,
      segment.control2.x,
      segment.control2.y,
      segment.end.x,
      segment.end.y,
    );
}
