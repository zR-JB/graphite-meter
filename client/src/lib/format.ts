// Formatting and scale helpers for speeds, bytes, latency, and chart domains.
import type { TerminationReason } from "./runner/contract";

export function reasonLabel(reason: TerminationReason): string {
  switch (reason) {
    case "preflight-failed":
      return "Couldn't reach the server";
    case "connection-lost":
      return "Connection lost";
    case "timeout":
      return "Connection timed out";
    case "protocol-error":
      return "Unexpected server response";
    case "transport-unavailable":
      return "Couldn't establish a connection";
    case "user-abort":
      return "Stopped";
    case "internal-error":
      return "Runner needs attention";
  }
}

export function fmtSpeed(value: number): string {
  if (value >= 1000) return value.toFixed(0);
  if (value >= 100) return value.toFixed(1);
  return value.toFixed(2);
}

export function fmtMs(ms: number): string {
  return ms < 100 ? ms.toFixed(1) : ms.toFixed(0);
}

export function fmtBytes(bytes: number, base: "base10" | "base2"): string {
  const step = base === "base10" ? 1000 : 1024;
  const units =
    base === "base10"
      ? ["B", "kB", "MB", "GB", "TB"]
      : ["B", "KiB", "MiB", "GiB", "TiB"];
  let tier = 0;
  let value = bytes;
  while (value >= step && tier < units.length - 1) {
    value /= step;
    tier++;
  }
  return `${value.toFixed(tier ? 1 : 0)} ${units[tier]}`;
}

type UnitBase = "base10" | "base2";
type UnitKind = "bits" | "bytes";

const SI_PREFIX = ["", "k", "M", "G", "T"];
const IEC_PREFIX = ["", "Ki", "Mi", "Gi", "Ti"];

export function rateUnit(base: UnitBase, kind: UnitKind, idx: number): string {
  const prefixes = base === "base10" ? SI_PREFIX : IEC_PREFIX;
  const prefix = prefixes[Math.max(0, Math.min(prefixes.length - 1, idx))];
  return kind === "bits" ? `${prefix}bit/s` : `${prefix}B/s`;
}

function unitDivisor(base: UnitBase, idx: number): number {
  const k = base === "base10" ? 1000 : 1024;
  return Math.pow(k, Math.max(0, Math.min(4, idx)));
}

export function rateScaleIndex(
  baseUnits: number,
  base: UnitBase,
  headroom = 1,
): number {
  // `headroom` delays prefix promotion.
  const k = base === "base10" ? 1000 : 1024;
  if (baseUnits < headroom) return 0;
  return Math.max(
    0,
    Math.min(4, Math.floor(Math.log(baseUnits / headroom) / Math.log(k))),
  );
}

/** Select the single display tier used by every throughput presentation. */
export function throughputUnitIndex(
  referenceBytesPerSec: number,
  base: UnitBase,
  kind: UnitKind,
): number {
  const baseUnits =
    kind === "bytes" ? referenceBytesPerSec : referenceBytesPerSec * 8;
  return rateScaleIndex(baseUnits, base, 1.2);
}

export function rateValueAt(
  bytesPerSec: number,
  base: UnitBase,
  kind: UnitKind,
  idx: number,
): number {
  const baseUnits = kind === "bytes" ? bytesPerSec : bytesPerSec * 8;
  return baseUnits / unitDivisor(base, idx);
}

export function rawRateFrom(
  displayValue: number,
  base: UnitBase,
  kind: UnitKind,
  idx: number,
): number {
  const baseUnits = displayValue * unitDivisor(base, idx);
  return kind === "bytes" ? baseUnits : baseUnits / 8;
}

/** The 100 Mbit/s reference used before automatic measurement has data. */
export const DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC = 12_500_000;

/** Select the linear chart's 1/2/5 ceiling. */
export function chartThroughputScale(peakBytesPerSec: number): number {
  // The scale is chosen in bit/s, then converted back to bytes/s. Bits and bytes displays share one visual ceiling.
  if (peakBytesPerSec <= 0) return DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC;
  return ceil125(peakBytesPerSec * 8) / 8;
}

export function niceCeil(value: number): number {
  if (value <= 0) return 1;
  return ceil125(value);
}

export function quantile(sorted: number[], q: number): number | null {
  if (!sorted.length) return null;
  if (sorted.length === 1) return sorted[0];
  const pos = (sorted.length - 1) * Math.min(1, Math.max(0, q));
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  const weight = pos - lo;
  return sorted[lo] * (1 - weight) + sorted[hi] * weight;
}

export function niceStep(span: number): number {
  if (span <= 0) return 1;
  const base = 10 ** Math.floor(Math.log10(span));
  const mantissa = span / base; // [1, 10)
  return (mantissa >= 5 ? 5 : mantissa >= 2 ? 2 : 1) * base;
}

interface NiceDomain {
  min: number;
  max: number;
  span: number;
}

export function niceDomain(
  values: number[],
  opts: {
    widen?: number;
    minSpanRatio?: number;
    floor?: number;
    clampMinZero?: boolean;
  } = {},
): NiceDomain {
  // Widens around the observed range. The minimum span keeps a flat series off the chart edge.
  const {
    widen = 1.35,
    minSpanRatio = 0.16,
    floor = 12,
    clampMinZero = true,
  } = opts;
  if (!values.length) return { min: 0, max: floor, span: floor };
  const rawMin = Math.min(...values);
  const rawMax = Math.max(...values);
  const rawSpan = Math.max(0, rawMax - rawMin);
  const weighted = Math.max(rawSpan * widen, rawMax * minSpanRatio, floor);
  const center = (rawMin + rawMax) / 2;
  const step = niceStep(weighted);
  let min = Math.floor((center - weighted / 2) / step) * step;
  if (clampMinZero) min = Math.max(0, min);
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
