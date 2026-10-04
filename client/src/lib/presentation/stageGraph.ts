import type { LatencyBucket, Phase } from "../runner/contract";
import { monotoneCurve } from "./smoothPath";

export interface GraphPoint {
  t: number;
  v: number;
}
export interface LatencyPoint {
  t: number;
  ms: number;
}

/** A stage's reply buckets as points; a bucket of timeouts has none. */
export const replies = (
  history: readonly LatencyBucket[],
  phase: Phase,
): LatencyPoint[] =>
  history.flatMap((b) =>
    b.phase === phase && b.medianRttMs !== null
      ? [{ t: b.t, ms: b.medianRttMs }]
      : [],
  );

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
  /** A reply over the track's top is clamped there and marked. */
  dots: { x: number; y: number; t: number; ms: number; over: boolean }[];
  baselineY: number | null;
  /** Per lane: bin centre times and rates, for the hover readout. */
  bins: GraphPoint[][];
}

const BIN_PX = 4;
const TOP_PAD = 3;

interface PlotPoint {
  x: number;
  y: number;
}
const curves = (points: PlotPoint[]) =>
  monotoneCurve(points).map(
    ({ control1: a, control2: b, end: e }) =>
      `C${a.x} ${a.y} ${b.x} ${b.y} ${e.x} ${e.y}`,
  );
const move = (point: PlotPoint) => `M${point.x} ${point.y}`;
const areaOf = (line: string, points: PlotPoint[], height: number) =>
  line ? `${line}L${points.at(-1)!.x} ${height}L${points[0].x} ${height}Z` : "";

export interface StageGraphGeometry extends StageGraph {
  points: PlotPoint[][];
  segments: string[][];
  prefixes: { count: number; line: string }[];
  x: (t: number) => number;
  y: (v: number) => number;
  plotHeight: number;
}

/** Lanes binned to the plot's width over a zero baseline; latency replies as dots around the idle median. */
export function stageGraphGeometry(
  input: Omit<StageGraphInput, "head">,
): StageGraphGeometry {
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
  const points = bins.map((lane) =>
    lane.map((p) => ({ x: x(p.t), y: y(p.v) })),
  );
  const segments = points.map(curves);
  const lines = points.map((lane, i) =>
    lane.length < 2 ? "" : move(lane[0]) + segments[i].join(""),
  );
  // One scale from 0 ms, so a reply below the idle median sits below its line.
  const trackY = (ms: number) =>
    trackHeight -
    1.5 -
    Math.min(1, Math.max(0, ms / (input.latencyTop || 1))) * (trackHeight - 3);
  // Replies bin to the width like the lanes, so every stage's track has bars of one pitch whatever its length; a
  // bin reads its buckets' mean and keeps the mark of any bucket over the top.
  const top = input.latencyTop || 1;
  const sums = new Float64Array(columns);
  const counts = new Uint16Array(columns);
  const overs = new Uint8Array(columns);
  for (const point of input.latency) {
    if (point.t < start || point.t > start + span) continue;
    const i = Math.min(
      columns - 1,
      Math.floor(((point.t - start) / (span || 1)) * columns),
    );
    sums[i] += point.ms;
    counts[i]++;
    if (point.ms > top) overs[i] = 1;
  }
  const dots = [...counts.keys()]
    .filter((i) => counts[i])
    .map((i) => {
      const t = start + ((i + 0.5) / columns) * span;
      const ms = sums[i] / counts[i];
      return { t, ms, x: x(t), y: trackY(ms), over: overs[i] === 1 };
    });
  return {
    lines,
    area: areaOf(lines[0] ?? "", points[0] ?? [], plotHeight),
    heads: [],
    dots,
    baselineY: input.baseline === null ? null : trackY(input.baseline),
    bins,
    points,
    segments,
    prefixes: points.map(() => ({ count: -1, line: "" })),
    x,
    y,
    plotHeight,
  };
}

/** Only the final two curve segments depend on the moving head; bins and latency dots stay cached. */
export function drawStageGraph(
  geometry: StageGraphGeometry,
  head: StageGraphInput["head"],
): StageGraph {
  if (!head) return geometry;
  const heads: PlotPoint[] = [];
  let area = geometry.area;
  const lines = geometry.points.map((points, lane) => {
    const value = head.values[lane];
    if (value == null || !points.length) return geometry.lines[lane];
    const tip = { x: geometry.x(head.t), y: geometry.y(value) };
    heads.push(tip);
    let n = points.length;
    while (n && points[n - 1].x >= tip.x) n--;
    const prefix = geometry.prefixes[lane];
    if (prefix.count !== n) {
      prefix.count = n;
      prefix.line = n
        ? move(points[0]) +
          geometry.segments[lane].slice(0, Math.max(0, n - 2)).join("")
        : "";
    }
    const tail = [...points.slice(Math.max(0, n - 3), n), tip];
    const line =
      n === 0
        ? ""
        : prefix.line +
          curves(tail)
            .slice(n >= 3 ? 1 : 0)
            .join("");
    if (lane === 0)
      area = line
        ? `${line}L${tip.x} ${geometry.plotHeight}L${points[0].x} ${geometry.plotHeight}Z`
        : "";
    return line;
  });
  return {
    lines,
    area,
    heads,
    dots: geometry.dots,
    baselineY: geometry.baselineY,
    bins: geometry.bins,
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
