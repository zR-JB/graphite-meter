// Every chart and gauge axis, derived from the presented series and results.
import type {
  LatencyBucket,
  Phase,
  RunResult,
  ThroughputResult,
  ThroughputSample,
} from "../runner/contract";
import { nearestRank } from "../runner/measure";
import { throughputUnitIndex, type UnitBase, type UnitKind } from "../format";

/** A live latency spike holds the axis this long. */
const LATENCY_WINDOW_MS = 8_000;
const LATENCY_HEADROOM = 1.25;
/** The highest plotted rate stays this far below the top edge. */
const RATE_HEADROOM = 1.03;
const RATE_STEPS = [1, 2, 5];
// The chart is read by its fill, so its ceiling sits just above the peak; the dial keeps round labels.
const CHART_RATE_STEPS = [1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8];
// Quarter ticks of a 1-2-4 ladder stay round: 0.25, 0.5, 1, 2.5, 5, 10 ms.
const LATENCY_STEPS = [1, 2, 4];
const LATENCY_FLOOR_MS = 1;
/* A track of sub-millisecond replies still shows their variation. */
const TRACK_FLOOR_MS = 0.1;
const EMPTY_LATENCY_MS = 20;

/** The 100 Mbit/s reference used before automatic measurement has data. */
export const DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC = 12_500_000;

/** The first ladder step at or above the value; every axis ceiling comes from here. */
export function ceilStep(value: number, steps: readonly number[]): number {
  if (!(value > 0) || !Number.isFinite(value)) return steps[0];
  const base = 10 ** Math.floor(Math.log10(value));
  const step = steps.find((step) => step * base >= value);
  return (step ?? 10) * base;
}

/** A throughput ceiling above the highest plotted rate on the shown unit's own ladder: bits or bytes, in powers
 *  of 1000 or 1024, so the dial's labels are round in whichever unit it reads. */
function rateCeiling(
  bytesPerSec: number,
  base: UnitBase,
  kind: UnitKind,
  steps = RATE_STEPS,
): number {
  const perByte = kind === "bits" ? 8 : 1;
  const value = bytesPerSec * perByte * RATE_HEADROOM;
  const k = base === "base10" ? 1000 : 1024;
  const tier = value >= 1 ? k ** Math.floor(Math.log(value) / Math.log(k)) : 1;
  return (ceilStep(value / tier, steps) * tier) / perByte;
}

/** The highest rate drawn on one lane, and the highest two concurrent lanes add up to. */
function peaks(series: readonly ThroughputSample[]) {
  let lane = 0;
  let combined = 0;
  for (let i = 0; i < series.length; i++) {
    const { t, bytesPerSec } = series[i];
    lane = Math.max(lane, bytesPerSec);
    const partner = series[i - 1]?.t === t ? series[i - 1].bytesPerSec : 0;
    combined = Math.max(combined, bytesPerSec + partner);
  }
  return { lane, combined };
}

const reported = (result: ThroughputResult | null | undefined) =>
  result?.reportedBytesPerSec ?? 0;

interface ThroughputScales {
  chartBytesPerSec: number;
  gaugeBytesPerSec: number;
  unitIndex: number;
}

/**
 * A manual maximum fixes every axis. Otherwise each axis contains everything it draws: the chart its lanes,
 * the gauge the combined rate. Peaks only grow during a run, so neither axis steps back mid-stage.
 */
export function throughputScales(
  series: readonly ThroughputSample[],
  result: Pick<RunResult, "download" | "upload" | "bidirectional">,
  manual: number | "auto",
  base: UnitBase,
  kind: UnitKind,
): ThroughputScales {
  if (manual !== "auto" && manual > 0)
    return {
      chartBytesPerSec: manual,
      gaugeBytesPerSec: manual,
      unitIndex: throughputUnitIndex(manual, base, kind),
    };
  const { lane, combined } = peaks(series);
  const { download, upload, bidirectional } = result;
  const down = reported(bidirectional?.down);
  const up = reported(bidirectional?.up);
  const single = Math.max(reported(download), reported(upload));
  const chartPeak = Math.max(lane, single, down, up);
  const gaugePeak = Math.max(combined, single, down + up);
  const unitIndex = throughputUnitIndex(
    gaugePeak || DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC,
    base,
    kind,
  );
  // From the mega tier up the dial starts at 1 Gbit/s, where most connections fit, on the unit's own ladder.
  const floor =
    unitIndex >= 2 ? rateCeiling(1e9 / 8 / RATE_HEADROOM, base, kind) : 0;
  return {
    chartBytesPerSec: chartPeak
      ? rateCeiling(chartPeak, base, kind, CHART_RATE_STEPS)
      : DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC,
    gaugeBytesPerSec: Math.max(floor, rateCeiling(gaugePeak, base, kind)),
    unitIndex,
  };
}

/** The latency ceiling at or above a value. */
const latencyCeiling = (ms: number) =>
  Math.max(LATENCY_FLOOR_MS, ceilStep(ms, LATENCY_STEPS));

const validMs = (values: readonly (number | null)[]) =>
  values
    .filter(
      (value): value is number =>
        value != null && Number.isFinite(value) && value >= 0,
    )
    .sort((a, b) => a - b);

/** The ladder tier above the p95 of reply medians, with headroom. */
export function latencyScale(medians: readonly (number | null)[]): number {
  const valid = validMs(medians);
  if (!valid.length) return EMPTY_LATENCY_MS;
  return latencyCeiling(nearestRank(valid, 0.95) * LATENCY_HEADROOM);
}

/** A reply track's top: the ladder tier above 2.5× the p75, so the body of the replies keeps its shape and an
 *  outlier is clamped with an arrow instead of flattening the rest. */
export function latencyTrackScale(medians: readonly (number | null)[]): number {
  const valid = validMs(medians);
  if (!valid.length) return EMPTY_LATENCY_MS;
  return Math.max(
    TRACK_FLOOR_MS,
    ceilStep(nearestRank(valid, 0.75) * 2.5, LATENCY_STEPS),
  );
}

/** The latency axis: the last 8 s while live, the whole series once finished. */
export function latencyAxisMs(
  history: readonly LatencyBucket[],
  finished: boolean,
): number {
  const from = finished
    ? -Infinity
    : (history.at(-1)?.endT ?? 0) - LATENCY_WINDOW_MS;
  const medians: (number | null)[] = [];
  for (let i = history.length - 1; i >= 0 && history[i].endT > from; i--)
    medians.push(history[i].medianRttMs);
  return latencyScale(medians);
}

/** The gauge shows the completed or live RTT on the shared axis once a bucket measured it. */
export function gaugeLatency(state: {
  phase: Phase;
  liveRttMs: number;
  axisMs: number;
  history: readonly LatencyBucket[];
  completedRttMs: number | null;
}): { rttMs: number; scaleMs: number } {
  const { phase, history, completedRttMs } = state;
  const completed = phase === "complete" && completedRttMs != null;
  const measured = completed
    ? history.some((bucket) => bucket.medianRttMs != null)
    : phase === "latency" && history.at(-1)?.phase === "latency";
  const rttMs = completed ? completedRttMs : state.liveRttMs;
  return { rttMs, scaleMs: measured ? state.axisMs : latencyScale([rttMs]) };
}
