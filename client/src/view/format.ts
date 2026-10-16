// Every number as text: rates in the page's unit, latency, bytes, durations and typed times, counts, shares, ranges,
// progress and dial ticks. A value that is missing or not finite reads MISSING with no unit, never NaN or Infinity.
import type { Preferences } from "../app/preferences";

export const MISSING = "–";

type UnitBase = Preferences["unitBase"];
type UnitKind = Preferences["unitKind"];
export interface Units {
  base: UnitBase;
  kind: UnitKind;
}
/** A page that sets `tier` (prefix index: 0 none, 1 k/Ki … 4 T/Ti) reads every rate in it, zero included. */
export interface RateUnits extends Units {
  tier?: number;
}
/** A figure and its unit, set apart in headlines. A missing value has no unit, so a unit never arrives before it. */
export interface Reading {
  figure: string;
  unit: string;
}

type Value = number | null | undefined;

const known = (value: Value): value is number => Number.isFinite(value);

const guarded =
  (format: (value: number) => string) =>
  (value: Value): string =>
    known(value) ? format(value) : MISSING;

/** Every figure rounds here, as `toFixed` does, but never reads as a negative zero or in exponent form: -0.001 reads
    0.00 and 1e21 in full. */
function fixed(value: number, digits: number): string {
  const text =
    Math.abs(value) < 1e21
      ? value.toFixed(digits)
      : value.toLocaleString("en-US", { useGrouping: false, minimumFractionDigits: digits });
  return text.startsWith("-") && Number(text) === 0 ? text.slice(1) : text;
}

/** A figure and its unit as one text; a missing figure stays a bare MISSING. */
export const withUnit = (figure: string, unit: string) => (figure === MISSING ? MISSING : `${figure} ${unit}`);

/** A rate's figure: two decimals below 100, one below 1000, none above, decided after rounding (99.996 → 100.0). */
export const fmtSpeed = guarded((value) =>
  fixed(
    value,
    Math.abs(Math.round(value * 100) / 100) < 100 ? 2 : Math.abs(Math.round(value * 10) / 10) < 1000 ? 1 : 0,
  ),
);

const fixedMs = (ms: number) => fixed(ms, Math.abs(Math.round(ms * 10) / 10) < 100 ? 1 : 0);

/** A latency figure: one decimal below 100 ms, decided after rounding. Browser timers resolve 0.1 ms, so a smaller
    measured value reads as below it. */
export const fmtMs = guarded((ms) => (ms >= 0 && ms < 0.1 ? "< 0.1" : fixedMs(ms)));

export const fmtLatency = (ms: Value) => withUnit(fmtMs(ms), "ms");

/** Fastest to slowest, both ends even when they read the same: 12.3–12.3 ms. */
export function fmtLatencyRange(fastest: Value, slowest: Value): string {
  const from = fmtMs(fastest);
  const to = fmtMs(slowest);
  return from === MISSING || to === MISSING ? MISSING : `${from}–${to} ms`;
}

/** The latency a load added, signed by its rounded figure, so a tiny reduction reads +0.0; a real one takes "−". */
export const fmtAddedMs = guarded((ms) => `${Number(fixedMs(ms)) < 0 ? "−" : "+"}${fixedMs(Math.abs(ms))}`);

const STEP: Record<UnitBase, number> = { base10: 1000, base2: 1024 };
const PREFIX: Record<UnitBase, readonly string[]> = {
  base10: ["", "k", "M", "G", "T"],
  base2: ["", "Ki", "Mi", "Gi", "Ti"],
};
const TOP_TIER = 4;
/** A prefix reads from 1.2× its step, so a rate just past one keeps the smaller unit: 1.1 Gbit/s reads 1100 Mbit/s. */
const PROMOTE_AT = 1.2;

const tierOf = (tier: number) => Math.min(TOP_TIER, Math.max(0, Math.round(tier) || 0));
const inKind = (bytesPerSec: number, kind: UnitKind) => (kind === "bits" ? bytesPerSec * 8 : bytesPerSec);

/** Bytes in the largest unit whose figure reaches 1, decided after rounding: 999.5 B reads 1.0 kB. */
export function fmtBytes(bytes: Value, base: UnitBase): string {
  if (!known(bytes)) return MISSING;
  let tier = 0;
  let value = bytes;
  while (tier < TOP_TIER && Number(fixed(value, tier ? 1 : 0)) >= STEP[base]) {
    value /= STEP[base];
    tier++;
  }
  return `${fixed(value, tier ? 1 : 0)} ${PREFIX[base][tier]}B`;
}

export const rateUnit = (units: Units, tier: number) =>
  `${PREFIX[units.base][tierOf(tier)]}${units.kind === "bits" ? "bit/s" : "B/s"}`;

/** The tier a rate reads in on its own: the largest unit it reaches 1.2 of, as the terminal clients judge it. */
export function rateTier(bytesPerSec: number, units: Units): number {
  const value = inKind(bytesPerSec, units.kind);
  const step = STEP[units.base];
  let tier = 0;
  while (tier < TOP_TIER && value / step ** tier >= PROMOTE_AT * step) tier++;
  return tier;
}

export const toRateUnit = (bytesPerSec: number, units: Units, tier: number) =>
  inKind(bytesPerSec, units.kind) / STEP[units.base] ** tierOf(tier);

/** Bytes per second of a value read in the unit of `tier`. */
export const fromRateUnit = (value: number, units: Units, tier: number) =>
  (value * STEP[units.base] ** tierOf(tier)) / (units.kind === "bits" ? 8 : 1);

/** A rate in the page's tier, or in its own when the page sets none. */
export function rateReading(bytesPerSec: Value, units: RateUnits): Reading {
  if (!known(bytesPerSec)) return { figure: MISSING, unit: "" };
  const tier = units.tier ?? rateTier(bytesPerSec, units);
  return { figure: fmtSpeed(toRateUnit(bytesPerSec, units, tier)), unit: rateUnit(units, tier) };
}

export function fmtRate(bytesPerSec: Value, units: RateUnits): string {
  const { figure, unit } = rateReading(bytesPerSec, units);
  return withUnit(figure, unit);
}

/** Whether `seconds` shown with `digits` decimals still reads below a minute: 59.96 s does not at one decimal. */
const belowMinute = (seconds: number, digits: number) => Number(fixed(seconds, digits)) < 60;
const pair = (large: number, unit: string, small: number, smallUnit: string) =>
  small ? `${large} ${unit} ${small} ${smallUnit}` : `${large} ${unit}`;

/** Seconds below a minute, then minutes and whole seconds, then hours and whole minutes: 59.9 s, 1 min 5 s, 2 h. */
export function fmtDuration(ms: Value, digits = 1): string {
  if (!known(ms)) return MISSING;
  const seconds = Math.max(0, ms) / 1000;
  if (belowMinute(seconds, digits)) return `${fixed(seconds, digits)} s`;
  const whole = Math.round(seconds);
  if (whole < 3600) return pair(Math.floor(whole / 60), "min", whole % 60, "s");
  // Hours count whole minutes, so 1 h 59.5 min reads 2 h, never 1 h 60 min.
  const minutes = Math.round(whole / 60);
  return pair(Math.floor(minutes / 60), "h", minutes % 60, "min");
}

/** A stage's planned time as the steppers show it: 0.5 s, 45 s, 1 min 30 s, 2 h. */
export const fmtStageTime = guarded((ms) => {
  const seconds = Math.max(0, ms) / 1000;
  return belowMinute(seconds, 1) ? `${Number(fixed(seconds, 1))} s` : fmtDuration(ms);
});

const UNIT_MS = { ms: 1, s: 1_000, m: 60_000, h: 3_600_000 };
const UNIT = /^(ms|s|sec|secs|seconds?|m|min|mins|minutes?|h|hr|hrs|hours?)$/;

/** A typed time in ms, or null: seconds unless a unit is named ("90", "2.5", "2h", "1 h 30 min", "500ms"), or a
    clock ("1:30", "1:30:00"). A shown stage time reads back as itself. */
export function parseDuration(text: string): number | null {
  const input = text.trim().toLowerCase();
  let total = 0;
  if (/^\d+(:\d{1,2}){1,2}$/.test(input)) {
    const parts = input.split(":").map(Number);
    if (parts.slice(1).some((part) => part >= 60)) return null;
    total = parts.reduce((sum, part) => sum * 60 + part) * 1000;
  } else {
    const tokens = [...input.matchAll(/(\d+(?:\.\d+)?)\s*([a-z]*)/g)];
    const bare = (value: string) => value.replace(/\s+/g, "");
    if (!tokens.length || bare(tokens.map(([token]) => token).join("")) !== bare(input)) return null;
    for (const [, amount, unit = ""] of tokens) {
      if (unit && !UNIT.test(unit)) return null;
      total += Number(amount) * UNIT_MS[unit === "ms" ? "ms" : ((unit[0] ?? "s") as "s" | "m" | "h")];
    }
  }
  return Number.isFinite(total) ? Math.round(total) : null;
}

/** Seconds into a stage over its length as the stepper shows it: "1.9 / 2.5 s", "65.3 / 5 min". The seconds keep
    counting past a minute, so the text never jumps at one. */
export function fmtProgress(elapsedMs: Value, totalMs: Value): string {
  if (!known(elapsedMs) || !known(totalMs)) return MISSING;
  return `${fixed(Math.max(0, elapsedMs) / 1000, 1)} / ${fmtStageTime(totalMs)}`;
}

// en-US like every fixed figure, so no page mixes separators.
const COUNT = new Intl.NumberFormat("en-US", { signDisplay: "negative" });

export const fmtCount = guarded((count) => COUNT.format(count));

/** A count and its noun, singular only when it reads 1: "1 reply", "1,204 replies". */
export function fmtCounted(count: Value, one: string, other = `${one}s`): string {
  const shown = fmtCount(count);
  return shown === MISSING ? MISSING : `${shown} ${shown === "1" ? one : other}`;
}

/** A share in percent as the terminal clients show it: two decimals below 1%, so a rare timeout stays legible, one
    from there: 0.25%, 12.5%, 100.0%. */
export const fmtPercent = guarded((pct) => `${fixed(pct, pct > 0 && pct < 1 ? 2 : 1)}%`);

/** A dial label: ungrouped, decimals down to the third significant figure, no trailing zeros: 0.25, 2.5, 1000. */
export const fmtTick = guarded((value) => {
  if (value === 0) return "0";
  const text = fixed(value, Math.min(100, Math.max(0, 2 - Math.floor(Math.log10(Math.abs(value))))));
  return text.includes(".") ? text.replace(/\.?0+$/, "") : text;
});
