// Every chart and dial axis: the throughput ceilings and the page's rate tier, and the latency ladder with its live
// window. Every ceiling is a step of a ladder that repeats each decade.
import type { LatencyBucket } from "../engine/measure/buckets";
import type { StageResults } from "../engine/run/result";
import type { TransferStage } from "../engine/run/schedule";
import { fromRateUnit, rateTier, type Units } from "./format";

/** The highest plotted rate stays this far below the top edge. */
const RATE_HEADROOM = 1.03;
const DIAL_STEPS = [1, 2, 5];
// The chart is read by its fill, so its ceiling sits just above the peak; the dial keeps round labels.
const CHART_STEPS = [1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8];
/** 100 Mbit/s stands in for a rate before any is known. */
const REFERENCE_BYTES_PER_SEC = 12_500_000;
const GIGABIT_BYTES_PER_SEC = 125_000_000;
/** Rates past 8 Pbit/s are held there, so a corrupt saved one cannot overflow an axis. */
const MAX_BYTES_PER_SEC = 1e15;
// Quarter ticks of a 1-2-4 ladder stay round: 0.25, 0.5, 1, 2.5, 5, 10 ms.
const LATENCY_STEPS = [1, 2, 4];
const LATENCY_HEADROOM = 1.25;
/** A track of sub-millisecond replies still shows their variation. */
const TRACK_FLOOR_MS = 0.1;
const EMPTY_LATENCY_MS = 20;
/** A live latency spike holds the axis this long. */
const LATENCY_WINDOW_MS = 8_000;

/** The first ladder step at or above `value`; 0 for none, so each axis applies its own floor. */
function ceilStep(value: number, steps: readonly number[]): number {
  if (!(value > 0) || !Number.isFinite(value)) return 0;
  const decade = 10 ** Math.floor(Math.log10(value));
  return (steps.find((step) => step * decade >= value) ?? 10) * decade;
}

/** The ladder step at or above `bytesPerSec` counted in the page's unit, so the dial's labels are round in the unit
    they read: bits or bytes, decimal or binary, 2000 Mibit/s rather than 2 Gibit/s read as 2048. */
function rateStep(bytesPerSec: number, units: Units, tier: number, steps: readonly number[]): number {
  const unit = fromRateUnit(1, units, tier);
  return ceilStep(Math.min(bytesPerSec, MAX_BYTES_PER_SEC) / unit, steps) * unit;
}

/** The fastest rates the shown run's live ticks drew, in bytes/s, kept as running maxima as the ticks arrive. */
interface LivePeaks {
  /** One lane alone, as the chart draws it. */
  lane: number;
  /** One tick's lanes together, as the dial shows them. */
  combined: number;
  /** The run has not ended. */
  running: boolean;
}

interface ThroughputScales {
  chartBytesPerSec: number;
  gaugeBytesPerSec: number;
  /** The page's one rate tier: every rate on it reads in this prefix. */
  tier: number;
}

/** A manual maximum fixes both axes exactly. Otherwise each axis holds everything it draws, with headroom: the chart
    each lane, the dial the lanes together. Peaks only grow during a run, so neither axis nor the tier steps back as a
    stage settles; a finished run reads in the tier its results set, as History reads it. */
export function throughputScales(
  results: Pick<StageResults, TransferStage>,
  live: LivePeaks | null,
  manual: number | "auto",
  units: Units,
): ThroughputScales {
  if (manual !== "auto" && manual > 0)
    return { chartBytesPerSec: manual, gaugeBytesPerSec: manual, tier: rateTier(manual, units) };
  const down = results.bidirectional?.down?.reportedBytesPerSec ?? 0;
  const up = results.bidirectional?.up?.reportedBytesPerSec ?? 0;
  const single = Math.max(results.download?.reportedBytesPerSec ?? 0, results.upload?.reportedBytesPerSec ?? 0);
  const settled = Math.max(single, down + up);
  const lane = Math.max(live?.lane ?? 0, single, down, up);
  const combined = Math.max(settled, live?.combined ?? 0);
  // A run stopped before any stage settled reads in the tier it was drawn in.
  const reference = live?.running || !settled ? combined : settled;
  const tier = rateTier(reference > 0 ? reference : REFERENCE_BYTES_PER_SEC, units);
  // From the mega tier up the dial starts at 1 Gbit/s, where most connections fit, on the unit's own ladder.
  const floor = tier >= 2 ? rateStep(GIGABIT_BYTES_PER_SEC, units, tier, DIAL_STEPS) : 0;
  return {
    chartBytesPerSec: lane > 0 ? rateStep(lane * RATE_HEADROOM, units, tier, CHART_STEPS) : REFERENCE_BYTES_PER_SEC,
    gaugeBytesPerSec: Math.max(floor, rateStep(combined * RATE_HEADROOM, units, tier, DIAL_STEPS)),
    tier,
  };
}

const ascending = (values: readonly (number | null)[]) =>
  values
    .filter((value): value is number => value !== null && Number.isFinite(value) && value >= 0)
    .sort((a, b) => a - b);
const nearestRank = (sorted: readonly number[], p: number) =>
  sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1))]!;

/** A latency axis: the ladder step over the p95 of `values` with headroom, at least 1 ms; 20 ms without values. */
export function latencyCeilingMs(values: readonly (number | null)[]): number {
  const sorted = ascending(values);
  if (!sorted.length) return EMPTY_LATENCY_MS;
  return Math.max(1, ceilStep(nearestRank(sorted, 0.95) * LATENCY_HEADROOM, LATENCY_STEPS));
}

/** A reply track's top: the ladder step over 2.5× the p75, so the body of the replies keeps its shape and an outlier
    is clamped with an arrow instead of flattening the rest. */
export function replyTrackMs(values: readonly (number | null)[]): number {
  const sorted = ascending(values);
  if (!sorted.length) return EMPTY_LATENCY_MS;
  return Math.max(TRACK_FLOOR_MS, ceilStep(nearestRank(sorted, 0.75) * 2.5, LATENCY_STEPS));
}

/** The shared latency axis over the buckets' medians: the last 8 s while live, all of them once finished. */
export function latencyAxisMs(buckets: readonly LatencyBucket[], finished: boolean): number {
  const from = finished ? -Infinity : (buckets.at(-1)?.endT ?? 0) - LATENCY_WINDOW_MS;
  const medians: (number | null)[] = [];
  for (let i = buckets.length - 1; i >= 0 && buckets[i]!.endT > from; i--) medians.push(buckets[i]!.medianRttMs);
  return latencyCeilingMs(medians);
}
