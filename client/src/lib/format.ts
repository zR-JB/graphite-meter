// Formatting and unit helpers for speeds, bytes, and latency.
import { MISSING } from "./presentation/vocabulary";

/** Every readout shows "—" for a value that is not a finite number, never "NaN" or "Infinity". */
const finite =
  <Rest extends unknown[]>(format: (value: number, ...rest: Rest) => string) =>
  (value: number, ...rest: Rest): string =>
    Number.isFinite(value) ? format(value, ...rest) : MISSING;

export const fmtSpeed = finite((value) => {
  if (Math.abs(Math.round(value * 100) / 100) < 100) return value.toFixed(2);
  return Math.abs(Math.round(value * 10) / 10) < 1000
    ? value.toFixed(1)
    : value.toFixed(0);
});

/** One decimal below 100 ms, decided after rounding so 99.96 reads 100. */
export const fixedMs = finite((ms) =>
  Math.abs(Math.round(ms * 10) / 10) < 100 ? ms.toFixed(1) : ms.toFixed(0),
);

/** Signed added latency; the sign follows the rounded value, so a tiny negative reads +0.0. */
export const fmtAddedMs = finite(
  (ms) => `${Number(fixedMs(ms)) < 0 ? "−" : "+"}${fixedMs(Math.abs(ms))}`,
);

/** Browser timers resolve 0.1 ms, so a smaller measured value is shown as below it. */
export const fmtMs = finite((ms) =>
  ms >= 0 && ms < 0.1 ? "< 0.1" : fixedMs(ms),
);

export const fmtMsTick = finite((ms) => (ms <= 0 ? "0" : fmtMs(ms)));

export const fmtDuration = finite((ms, fractionDigits: number = 1) => {
  const seconds = Math.max(0, ms) / 1000;
  if (seconds < 59.95) return `${seconds.toFixed(fractionDigits)} s`;
  const whole = Math.round(seconds);
  const [large, small, unit, rest] =
    whole < 3600
      ? [Math.floor(whole / 60), whole % 60, "min", "s"]
      : [Math.floor(whole / 3600), Math.round((whole % 3600) / 60), "h", "min"];
  return `${large} ${unit}${small ? ` ${small} ${rest}` : ""}`;
});

export const fmtBytes = finite((bytes, base: "base10" | "base2") => {
  const step = base === "base10" ? 1000 : 1024;
  const units =
    base === "base10"
      ? ["B", "kB", "MB", "GB", "TB"]
      : ["B", "KiB", "MiB", "GiB", "TiB"];
  let tier = 0;
  let value = bytes;
  while (+value.toFixed(tier ? 1 : 0) >= step && tier < units.length - 1) {
    value /= step;
    tier++;
  }
  return `${value.toFixed(tier ? 1 : 0)} ${units[tier]}`;
});

export type UnitBase = "base10" | "base2";
export type UnitKind = "bits" | "bytes";

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

/** A result rate chooses its own unit tier by the live rule, never a run's chart tier. */
export function resultRate(
  bytesPerSec: number,
  units: { base: UnitBase; kind: UnitKind },
) {
  const tier = throughputUnitIndex(bytesPerSec, units.base, units.kind);
  return {
    num: fmtSpeed(rateValueAt(bytesPerSec, units.base, units.kind, tier)),
    unit: rateUnit(units.base, units.kind, tier),
  };
}
