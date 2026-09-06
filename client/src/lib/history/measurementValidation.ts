import type { ReflectorTimingSummary } from "../runner/contract";

const nonnegative = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value) && value >= 0;
const percentage = (value: unknown): value is number =>
  nonnegative(value) && value <= 100;
const unitInterval = (value: unknown): value is number =>
  nonnegative(value) && value <= 1;
const method = (value: unknown) =>
  value === "stable-window" || value === "full-average";
const band = (value: unknown) =>
  value === "low" || value === "medium" || value === "high";

// Both aggregate snapshots and per-server results preserve these units and bounds.
export function hasThroughputMeasurements(
  value: Record<string, unknown>,
): boolean {
  return (
    nonnegative(value.reportedBytesPerSec) &&
    nonnegative(value.peakBytesPerSec) &&
    nonnegative(value.fullAverageBytesPerSec) &&
    nonnegative(value.totalBytes) &&
    percentage(value.stabilityPct) &&
    (value.probeTimeoutPct === null || percentage(value.probeTimeoutPct)) &&
    unitInterval(value.stabilityScore) &&
    method(value.method) &&
    band(value.band)
  );
}

export function hasLatencyMeasurements(
  value: Record<string, unknown>,
): boolean {
  return (
    nonnegative(value.reportedMs) &&
    ["minMs", "p50Ms", "p95Ms", "jitterMs"].every(
      (key) => value[key] === null || nonnegative(value[key]),
    ) &&
    (value.probeTimeoutPct === null || percentage(value.probeTimeoutPct)) &&
    unitInterval(value.stabilityScore) &&
    method(value.method) &&
    band(value.band)
  );
}

export function isReflectorTimingSummary(
  value: unknown,
  replies: number,
): value is ReflectorTimingSummary {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const timing = value as Record<string, unknown>;
  return (
    Object.keys(timing).every((key) =>
      [
        "sampleCount",
        "meanRawRttMs",
        "meanHandlingMs",
        "meanAdjustedRttMs",
      ].includes(key),
    ) &&
    Number.isSafeInteger(timing.sampleCount) &&
    nonnegative(timing.sampleCount) &&
    timing.sampleCount > 0 &&
    timing.sampleCount <= replies &&
    nonnegative(timing.meanRawRttMs) &&
    nonnegative(timing.meanHandlingMs) &&
    nonnegative(timing.meanAdjustedRttMs) &&
    timing.meanHandlingMs <= timing.meanRawRttMs &&
    timing.meanAdjustedRttMs <= timing.meanRawRttMs &&
    Math.abs(
      timing.meanRawRttMs - timing.meanHandlingMs - timing.meanAdjustedRttMs,
    ) <=
      1e-8 * Math.max(1, timing.meanRawRttMs)
  );
}
