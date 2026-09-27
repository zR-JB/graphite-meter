import { monotoneCurve } from "./smoothPath";

export interface GraphPoint {
  t: number;
  v: number;
}
export interface LatencyPoint {
  t: number;
  ms: number;
}

export interface StageGraphInput {
  /** One lane per direction, `t` in ms on the run's timeline; bidirectional has two. */
  lanes: GraphPoint[][];
  latency: LatencyPoint[];
  start: number;
  /** Ms the time axis covers: the plan while running, the measured span once settled. */
  span: number;
  /** Bytes/s at the top of the plot, shared by every card so heights compare. */
  ceiling: number;
  /** The idle median, drawn across the track: a reply's height over it is the latency its load added. */
  baseline: number | null;
  /** Ms at the top of the latency track, shared by every card. */
  latencyTop: number;
  width: number;
  plotHeight: number;
  trackHeight: number;
  /** The live leading edge: now on the timeline and each lane's glided rate. */
  head?: { t: number; values: (number | null)[] } | null;
}

export interface StageGraph {
  lines: string[];
  area: string;
  heads: { x: number; y: number }[];
  dots: { x: number; y: number; t: number; ms: number }[];
  baselineY: number | null;
  /** Per lane: bin centre times and rates, for the hover readout. */
  bins: GraphPoint[][];
}

const BIN_PX = 4;
const TOP_PAD = 3;

const path = (points: { x: number; y: number }[]) =>
  points.length < 2
    ? ""
    : `M${points[0].x} ${points[0].y}` +
      monotoneCurve(points)
        .map(
          ({ control1: a, control2: b, end: e }) =>
            `C${a.x} ${a.y} ${b.x} ${b.y} ${e.x} ${e.y}`,
        )
        .join("");

/** Lanes binned to the plot's width over a zero baseline; latency replies as dots around the idle median. */
export function stageGraph(input: StageGraphInput): StageGraph {
  const { start, span, width, plotHeight, trackHeight } = input;
  const columns = Math.max(8, Math.floor(width / BIN_PX));
  const x = (t: number) =>
    Math.min(width, Math.max(0, ((t - start) / (span || 1)) * width));
  const y = (v: number) =>
    plotHeight -
    Math.min(1, Math.max(0, v / (input.ceiling || 1))) * (plotHeight - TOP_PAD);
  const bins = input.lanes.map((points) => {
    const sums = new Float64Array(columns);
    const counts = new Uint16Array(columns);
    for (const { t, v } of points) {
      if (t < start) continue;
      const i = Math.min(
        columns - 1,
        Math.floor(((t - start) / (span || 1)) * columns),
      );
      sums[i] += v;
      counts[i]++;
    }
    return [...counts.keys()]
      .filter((i) => counts[i])
      .map((i) => ({
        t: start + ((i + 0.5) / columns) * span,
        v: sums[i] / counts[i],
      }));
  });
  const heads = input.head
    ? input.head.values.flatMap((value, lane) =>
        value == null || !bins[lane]?.length
          ? []
          : [{ lane, x: x(input.head!.t), y: y(value) }],
      )
    : [];
  const drawn = bins.map((lane, index) => {
    const points = lane.map((point) => ({ x: x(point.t), y: y(point.v) }));
    const head = heads.find((h) => h.lane === index);
    if (head && head.x > (points.at(-1)?.x ?? -1)) points.push(head);
    return points;
  });
  const first = drawn[0] ?? [];
  const area =
    first.length < 2
      ? ""
      : `${path(first)}L${first.at(-1)!.x} ${plotHeight}L${first[0].x} ${plotHeight}Z`;
  // One scale from 0 ms, so a reply below the idle median sits below its line.
  const trackY = (ms: number) =>
    trackHeight -
    1.5 -
    Math.min(1, Math.max(0, ms / (input.latencyTop || 1))) * (trackHeight - 3);
  const dots = input.latency
    .filter((point) => point.t >= start && point.t <= start + span)
    .map((point) => ({ ...point, x: x(point.t), y: trackY(point.ms) }));
  return {
    lines: drawn.map(path),
    area,
    heads: heads.map(({ x, y }) => ({ x, y })),
    dots,
    baselineY: input.baseline === null ? null : trackY(input.baseline),
    bins,
  };
}

/** The bin and reply nearest a time, for the readout under the pointer. */
export function nearestAt<T extends { t: number }>(
  points: readonly T[],
  t: number,
): T | null {
  let best: T | null = null;
  for (const point of points)
    if (!best || Math.abs(point.t - t) < Math.abs(best.t - t)) best = point;
  return best;
}
