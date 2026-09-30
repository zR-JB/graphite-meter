// Formatting and unit helpers for speeds, bytes, and latency.
import { MISSING } from "./presentation/vocabulary";

/** Every readout shows "—" for a value that is not a finite number, never "NaN" or "Infinity". */
const finite =
  <Rest extends unknown[]>(format: (value: number, ...rest: Rest) => string) =>
  (value: number, ...rest: Rest): string =>
    Number.isFinite(value) ? format(value, ...rest) : MISSING;

const countFormat = new Intl.NumberFormat("en-US");
export const fmtCount = (count: number) => countFormat.format(count);
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

export const fmtDuration = finite((ms, fractionDigits: number = 1) => {
  const seconds = Math.max(0, ms) / 1000;
  if (seconds < 59.95) return `${seconds.toFixed(fractionDigits)} s`;
  const whole = Math.round(seconds);
  // Hours count whole minutes, so 1 h 59.5 min reads 2 h, never 1 h 60 min.
  const minutes = Math.round(whole / 60);
  const [large, small, unit, rest] =
    whole < 3600
      ? [Math.floor(whole / 60), whole % 60, "min", "s"]
      : [Math.floor(minutes / 60), minutes % 60, "h", "min"];
  return `${large} ${unit}${small ? ` ${small} ${rest}` : ""}`;
});

/** A stage's planned time as the steppers and the duration strip show it: 2.5 s, 45 s, 1 min 30 s, 2 h. */
export const fmtStageTime = finite((ms) =>
  ms < 59_950
    ? `${Number((Math.max(0, ms) / 1000).toFixed(1))} s`
    : fmtDuration(ms),
);

const UNIT_MS: Record<string, number> = {
  ms: 1,
  s: 1_000,
  m: 60_000,
  h: 3_600_000,
};
const UNIT = /^(ms|s|sec|secs|seconds?|m|min|mins|minutes?|h|hr|hrs|hours?)$/;

/** A typed stage time: seconds by default ("90", "2.5"), units ("2h", "1 h 30 min", "90s") or a clock ("1:30:00"). */
export function parseDuration(text: string): number | null {
  const input = text.trim().toLowerCase();
  if (/^\d+(:\d{1,2}){1,2}$/.test(input)) {
    const parts = input.split(":").map(Number);
    if (parts.slice(1).some((part) => part >= 60)) return null;
    return parts.reduce((total, part) => total * 60 + part, 0) * 1_000;
  }
  const tokens = [...input.matchAll(/(\d+(?:\.\d+)?)\s*([a-z]*)/g)];
  const bare = (value: string) => value.replace(/\s+/g, "");
  if (
    !tokens.length ||
    bare(tokens.map(([token]) => token).join("")) !== bare(input)
  )
    return null;
  let total = 0;
  for (const [, amount, unit] of tokens) {
    if (unit && !UNIT.test(unit)) return null;
    total += Number(amount) * UNIT_MS[unit === "ms" ? "ms" : (unit[0] ?? "s")];
  }
  return Number.isFinite(total) ? Math.round(total) : null;
}

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

/** A page that sets `tier` reads every rate in that one unit, zero included. */
export type RateUnits = { base: UnitBase; kind: UnitKind; tier?: number };

/** A rate in the page's tier, else its own by the live rule, unless a chart gives its tier. */
export function resultRate(
  bytesPerSec: number,
  units: RateUnits,
  tier = units.tier ?? throughputUnitIndex(bytesPerSec, units.base, units.kind),
) {
  return {
    num: fmtSpeed(rateValueAt(bytesPerSec, units.base, units.kind, tier)),
    unit: rateUnit(units.base, units.kind, tier),
  };
}

export function formatRate(
  bytesPerSec: number | null | undefined,
  units: RateUnits,
  tier?: number,
): string {
  if (bytesPerSec == null) return MISSING;
  const { num, unit } = resultRate(bytesPerSec, units, tier);
  return `${num} ${unit}`;
}

export const formatLatency = (ms: number | null | undefined): string =>
  ms == null ? MISSING : `${fmtMs(ms)} ms`;
