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

/** A rate sets the throughput axis only once it has held this long. */
const SUSTAIN_MS = 700;
/** A live latency spike holds the axis this long. */
const LATENCY_WINDOW_MS = 8_000;
const LATENCY_HEADROOM = 1.25;
const LATENCY_LADDER_MS = [20, 40, 100, 200, 400, 1_000, 2_000, 4_000];

/** The 100 Mbit/s reference used before automatic measurement has data. */
export const DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC = 12_500_000;

/** Select the linear chart's 1/2/5 ceiling. */
function chartThroughputScale(peakBytesPerSec: number): number {
  // The scale is chosen in bit/s, then converted back to bytes/s. Bits and bytes displays share one visual ceiling.
  if (peakBytesPerSec <= 0) return DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC;
  return ceil125(peakBytesPerSec * 8) / 8;
}

function niceStep(span: number): number {
  if (span <= 0) return 1;
  const base = 10 ** Math.floor(Math.log10(span));
  const mantissa = span / base; // [1, 10)
  return (mantissa >= 5 ? 5 : mantissa >= 2 ? 2 : 1) * base;
}

/** A value range: min is the left edge, span its width in the metric's own units. */
export interface NiceDomain {
  min: number;
  max: number;
  span: number;
}

/** A 1-2-5 domain around the values; the minimum span keeps a flat series off the chart edge. */
export function niceDomain(values: number[], floor = 12): NiceDomain {
  if (!values.length) return { min: 0, max: floor, span: floor };
  const rawMin = Math.min(...values);
  const rawMax = Math.max(...values);
  const rawSpan = Math.max(0, rawMax - rawMin);
  const weighted = Math.max(rawSpan * 1.35, rawMax * 0.16, floor);
  const center = (rawMin + rawMax) / 2;
  const step = niceStep(weighted);
  const min = Math.max(0, Math.floor((center - weighted / 2) / step) * step);
  const max = Math.ceil((center + weighted / 2) / step) * step;
  const span = Math.max(step, max - min);
  return { min, max: min + span, span };
}

function ceil125(value: number): number {
  const base = 10 ** Math.floor(Math.log10(value));
  const mantissa = value / base;
  return (
    (mantissa <= 1 ? 1 : mantissa <= 2 ? 2 : mantissa <= 5 ? 5 : 10) * base
  );
}

/** Select the gauge ceiling independently from the chart ceiling: the next decade in bit/s, above an optional floor. */
export function gaugeScaleForPeak(
  peakBytesPerSec: number,
  floorBitsPerSec = 0,
): number {
  const bits = Math.max(0, peakBytesPerSec) * 8;
  const decade =
    Number.isFinite(bits) && bits > 0 ? 10 ** Math.ceil(Math.log10(bits)) : 1;
  return Math.max(floorBitsPerSec, decade) / 8;
}

let times = new Float64Array(256);
let rates = new Float64Array(256);

/** The highest combined rate held for 700 ms (or since the series began), and the highest one seen at all. */
function peaks(series: readonly ThroughputSample[]) {
  if (times.length < series.length) {
    times = new Float64Array(series.length * 2);
    rates = new Float64Array(series.length * 2);
  }
  let n = 0;
  for (const sample of series)
    if (n && times[n - 1] === sample.t) rates[n - 1] += sample.bytesPerSec;
    else {
      times[n] = sample.t;
      rates[n++] = sample.bytesPerSec;
    }
  let sustained = 0;
  let raw = 0;
  for (let i = 0, start = 0; i < n; i++) {
    raw = Math.max(raw, rates[i]);
    while (start < i && times[start + 1] <= times[i] - SUSTAIN_MS) start++;
    let low = rates[i];
    for (let j = start; j < i; j++) low = Math.min(low, rates[j]);
    sustained = Math.max(sustained, low);
  }
  return { sustained, raw };
}

const reported = (result: ThroughputResult | null | undefined) =>
  result?.reportedBytesPerSec ?? 0;

interface ThroughputScales {
  chartBytesPerSec: number;
  gaugeBytesPerSec: number;
  unitIndex: number;
}

/** A manual maximum fixes every axis; otherwise sustained rates and results set them. */
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
      gaugeBytesPerSec: gaugeScaleForPeak(manual),
      unitIndex: throughputUnitIndex(manual, base, kind),
    };
  const { sustained, raw } = peaks(series);
  const bidirectional = result.bidirectional;
  const peak = Math.max(
    sustained,
    reported(result.download),
    reported(result.upload),
    reported(bidirectional?.down) + reported(bidirectional?.up),
  );
  const unitIndex = throughputUnitIndex(
    raw || DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC,
    base,
    kind,
  );
  // The dial never shows a rate off its scale; from the mega tier up it starts at 1 Gbit/s.
  const floor = unitIndex >= 2 ? 1_000_000_000 : 0;
  return {
    chartBytesPerSec: chartThroughputScale(peak),
    gaugeBytesPerSec: gaugeScaleForPeak(Math.max(peak, raw), floor),
    unitIndex,
  };
}

/** The ladder tier above the p95 of reply medians, with headroom. */
export function latencyScale(medians: readonly (number | null)[]): number {
  const valid = medians
    .filter(
      (value): value is number =>
        value != null && Number.isFinite(value) && value >= 0,
    )
    .sort((a, b) => a - b);
  if (!valid.length) return LATENCY_LADDER_MS[0];
  const target = nearestRank(valid, 0.95) * LATENCY_HEADROOM;
  const tier = LATENCY_LADDER_MS.find((value) => value >= target);
  if (tier) return tier;
  const exponent = 10 ** Math.floor(Math.log10(target));
  return [1, 2, 5, 10].find((step) => step * exponent >= target)! * exponent;
}

/** True when any drawn part of a bucket exceeds the domain. */
export function latencyBucketExceedsScale(
  bucket: LatencyBucket,
  scaleMs: number,
): boolean {
  return [bucket.medianRttMs, bucket.p95RttMs, bucket.maxRttMs].some(
    (value) => value != null && value > scaleMs,
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
