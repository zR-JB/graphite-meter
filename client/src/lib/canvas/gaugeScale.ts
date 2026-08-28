import { rateScaleIndex, rateUnit, rateValueAt, rawRateFrom } from "../format";

// Throughput values use one low-end transfer curve for both directions.
export const THROUGHPUT_VALUE_KNOTS = [
  0, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 1,
] as const;
export const THROUGHPUT_FRACTION_KNOTS = THROUGHPUT_VALUE_KNOTS.map(
  (_, index) => index / (THROUGHPUT_VALUE_KNOTS.length - 1),
);

const AUTO_GAUGE_FLOOR_BITS_PER_SEC = 1_000_000_000;

function clamp01(value: number): number {
  return Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0;
}

function safeScale(scale: number): number {
  return Number.isFinite(scale) && scale > 0 ? scale : 1;
}

/** Map a raw throughput value to the uniform-fraction gauge domain. */
export function throughputGaugeFraction(value: number, scale: number): number {
  const normalized = clamp01(value / safeScale(scale));
  for (let index = 1; index < THROUGHPUT_VALUE_KNOTS.length; index++) {
    const upper = THROUGHPUT_VALUE_KNOTS[index]!;
    if (normalized <= upper) {
      const lower = THROUGHPUT_VALUE_KNOTS[index - 1]!;
      const span = upper - lower;
      const weight = span > 0 ? (normalized - lower) / span : 0;
      return (
        THROUGHPUT_FRACTION_KNOTS[index - 1]! +
        weight *
          (THROUGHPUT_FRACTION_KNOTS[index]! -
            THROUGHPUT_FRACTION_KNOTS[index - 1]!)
      );
    }
  }
  return 1;
}

/** Inverse of throughputGaugeFraction, using the same transfer knots. */
export function throughputValueAtFraction(
  fraction: number,
  scale: number,
): number {
  const normalizedFraction = clamp01(fraction);
  for (let index = 1; index < THROUGHPUT_FRACTION_KNOTS.length; index++) {
    const upper = THROUGHPUT_FRACTION_KNOTS[index]!;
    if (normalizedFraction <= upper) {
      const lower = THROUGHPUT_FRACTION_KNOTS[index - 1]!;
      const span = upper - lower;
      const weight = span > 0 ? (normalizedFraction - lower) / span : 0;
      return (
        safeScale(scale) *
        (THROUGHPUT_VALUE_KNOTS[index - 1]! +
          weight *
            (THROUGHPUT_VALUE_KNOTS[index]! -
              THROUGHPUT_VALUE_KNOTS[index - 1]!))
      );
    }
  }
  return safeScale(scale);
}

/** Values at the nine fixed angular ticks, including unlabeled interior ticks. */
export function throughputTickValues(scale: number): number[] {
  return THROUGHPUT_FRACTION_KNOTS.map((fraction) =>
    throughputValueAtFraction(fraction, scale),
  );
}

/** The fixed tick fractions shared by canvas geometry and throughput values. */
export function throughputTickFractions(_scale: number): number[] {
  return [...THROUGHPUT_FRACTION_KNOTS];
}

function decimalCeiling(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return 1;
  return 10 ** Math.ceil(Math.log10(value));
}

/** Select the gauge ceiling independently from the chart ceiling. */
export function gaugeScaleForPeak(
  peakBytesPerSec: number,
  automatic: boolean,
): number {
  const peakBitsPerSec = Math.max(0, peakBytesPerSec) * 8;
  const bitsPerSec = automatic
    ? Math.max(AUTO_GAUGE_FLOOR_BITS_PER_SEC, decimalCeiling(peakBitsPerSec))
    : decimalCeiling(peakBitsPerSec);
  return bitsPerSec / 8;
}

/** Prefix index that keeps the smallest nonzero labeled tick at >= 1. */
export function gaugeUnitIndex(
  scaleBytesPerSec: number,
  base: "base10" | "base2",
  kind: "bits" | "bytes",
): number {
  const scale = safeScale(scaleBytesPerSec);
  let index = rateScaleIndex(kind === "bits" ? scale * 8 : scale, base, 1);
  while (
    index > 0 &&
    rateValueAt(throughputValueAtFraction(0.25, scale), base, kind, index) < 1
  )
    index--;
  return index;
}

export function gaugeUnitLabel(
  scaleBytesPerSec: number,
  base: "base10" | "base2",
  kind: "bits" | "bytes",
): string {
  return rateUnit(base, kind, gaugeUnitIndex(scaleBytesPerSec, base, kind));
}

export function gaugeRateValue(
  bytesPerSec: number,
  scaleBytesPerSec: number,
  base: "base10" | "base2",
  kind: "bits" | "bytes",
): number {
  return rateValueAt(
    bytesPerSec,
    base,
    kind,
    gaugeUnitIndex(scaleBytesPerSec, base, kind),
  );
}

export function gaugeRateFrom(
  displayValue: number,
  scaleBytesPerSec: number,
  base: "base10" | "base2",
  kind: "bits" | "bytes",
): number {
  return rawRateFrom(
    displayValue,
    base,
    kind,
    gaugeUnitIndex(scaleBytesPerSec, base, kind),
  );
}

/** Compact, ungrouped labels with bounded meaningful precision. */
export function fmtGaugeTick(value: number): string {
  if (!Number.isFinite(value) || value === 0) return "0";
  const places = Math.min(
    6,
    Math.max(0, 2 - Math.floor(Math.log10(Math.abs(value)))),
  );
  const text = value.toFixed(places);
  return text.includes(".") ? text.replace(/0+$/, "").replace(/\.$/, "") : text;
}
