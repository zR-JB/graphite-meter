/* ============================================================
 * The Graphite Meter — Format Utilities (§10)
 * Guarantee the "zero layout shift" mandate. All numeric
 * renders pass through here.
 * ============================================================ */

import type { TerminationReason } from "./runner/contract";

/** Friendly phrasing for a structured failure / stall reason — the single
 *  source of user-facing copy for a TerminationReason (gauge status, toast).
 *  Backend `error.message` strings are engineering detail (stream indices,
 *  timeout names) and must never reach the UI verbatim. */
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

/** Convert raw bytesPerSec → display value in active unit (mirrors store.toUnit). */
export function toDisplaySpeed(
  bytesPerSec: number,
  base: "base10" | "base2",
  kind: "bits" | "bytes",
): number {
  const div = base === "base10" ? 1e6 : 2 ** 20;
  return kind === "bytes" ? bytesPerSec / div : (bytesPerSec * 8) / div;
}

/** Fixed-width formatting: always returns same char count for a magnitude band
 *  so tabular-nums + this guarantee zero reflow. */
export function fmtSpeed(v: number): string {
  if (v >= 1000) return v.toFixed(0); // 1000+ → integer
  if (v >= 100) return v.toFixed(1); // 100.0
  return v.toFixed(2); // 12.34
}

export function fmtMs(ms: number): string {
  return ms < 100 ? ms.toFixed(1) : ms.toFixed(0);
}

export function fmtBytes(b: number, base: "base10" | "base2"): string {
  const k = base === "base10" ? 1000 : 1024;
  // Official notation: SI decimal uses lowercase k (kB); IEC binary uses KiB.
  const u = base === "base10" ? ["B", "kB", "MB", "GB", "TB"] : ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  let n = b;
  while (n >= k && i < u.length - 1) {
    n /= k;
    i++;
  }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}

/* ============================================================
 * Rate units — official notation (IEC 80000-13 / SI).
 *   bits, decimal:  bit/s · kbit/s · Mbit/s · Gbit/s · Tbit/s
 *   bits, binary:   bit/s · Kibit/s · Mibit/s · Gibit/s · Tibit/s
 *   bytes, decimal: B/s · kB/s · MB/s · GB/s · TB/s
 *   bytes, binary:  B/s · KiB/s · MiB/s · GiB/s · TiB/s
 * (lowercase k for kilo; uppercase Ki for kibi). Internal values are
 * always bytes-per-second (browser-native); these convert + label for display.
 * ============================================================ */
export type UnitBase = "base10" | "base2";
export type UnitKind = "bits" | "bytes";

const SI_PREFIX = ["", "k", "M", "G", "T"];
const IEC_PREFIX = ["", "Ki", "Mi", "Gi", "Ti"];

/** Official unit symbol at a prefix index (0 = base … 4 = T/Ti). */
export function rateUnit(base: UnitBase, kind: UnitKind, idx: number): string {
  const prefixes = base === "base10" ? SI_PREFIX : IEC_PREFIX;
  const p = prefixes[Math.max(0, Math.min(prefixes.length - 1, idx))];
  return kind === "bits" ? `${p}bit/s` : `${p}B/s`;
}

/** Divisor (in base units) for a prefix index. */
function unitDivisor(base: UnitBase, idx: number): number {
  const k = base === "base10" ? 1000 : 1024;
  return Math.pow(k, Math.max(0, Math.min(4, idx)));
}

/** Prefix index for a magnitude expressed in base units (bit/s or B/s).
 *  `headroom` (≥1) delays stepping up to the next prefix until the magnitude
 *  is that factor past the boundary — so the unit only changes once we're
 *  comfortably above the threshold (e.g. ≥1.2 Gbit/s rather than at 1.0), never
 *  landing on a 0.xx reading of the larger unit. */
export function rateScaleIndex(baseUnits: number, base: UnitBase, headroom = 1): number {
  const k = base === "base10" ? 1000 : 1024;
  if (baseUnits < headroom) return 0;
  return Math.max(0, Math.min(4, Math.floor(Math.log(baseUnits / headroom) / Math.log(k))));
}

/** Convert raw bytes/s → display value at an explicit prefix index. */
export function rateValueAt(bytesPerSec: number, base: UnitBase, kind: UnitKind, idx: number): number {
  const baseUnits = kind === "bytes" ? bytesPerSec : bytesPerSec * 8;
  return baseUnits / unitDivisor(base, idx);
}

/** Inverse of `rateValueAt`: a display value in the active unit → raw bytes/s.
 *  Used when the UI writes a user-entered ceiling back into the (bytes-native)
 *  config, so the round-trip is lossless at the current prefix. */
export function rawRateFrom(displayValue: number, base: UnitBase, kind: UnitKind, idx: number): number {
  const baseUnits = displayValue * unitDivisor(base, idx);
  return kind === "bytes" ? baseUnits : baseUnits / 8;
}

/** Absolute throughput ceiling (bytes/s) for an observed peak (bytes/s). The
 *  SINGLE source of scale for BOTH the gauge dial and the chart Y-axis, so the
 *  two are always identically scaled (a glance at either maps to the same tier).
 *
 *  Tiers are a 1-2-5 ladder reasoned about in bit/s — the unit people quote links
 *  in — so the ceiling is always a round, recognizable rung: 10 / 20 / 50 / 100 /
 *  200 / 500 Mbit, 1 / 2 / 5 Gbit … up to data-center rates. The 1-2-5 ladder
 *  rounds UP to the next rung above the peak: at most ~2.5× headroom (vs. 10× for
 *  a one-per-decade ladder), so the needle/line fills the instrument well while
 *  still leaving room before any rescale.
 *
 *  This flatters the common case: real links deliver just UNDER their rated power
 *  (a "1 Gbit" line sustains ~940 Mbit after overhead), so the peak rounds up to
 *  the exactly-named rung and reads ~94% (940 Mbit → 1 Gbit). A line that
 *  over-delivers past a rung steps to the next 1-2-5 rung (e.g. 1.2 Gbit → 2 Gbit)
 *  rather than leaping a whole decade. Pair with a sustained (dwell-filtered) peak
 *  so a brief transient can't push the tier — see store.displayScaleBytesPerSec. */
export function sharedThroughputScale(peakBytesPerSec: number): number {
  if (peakBytesPerSec <= 0) return 1.25e7; // idle default: 100 Mbit/s so ticks render
  const peakBits = peakBytesPerSec * 8;
  const exp = Math.floor(Math.log10(peakBits));
  const base = 10 ** exp;
  const f = peakBits / base; // 1 ≤ f < 10
  const tierF = f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10; // next 1-2-5 rung up
  return (tierF * base) / 8; // bit/s → byte/s
}

/** "Nice" axis ceiling for charts. */
export function niceCeil(v: number): number {
  if (v <= 0) return 1;
  const exp = Math.floor(Math.log10(v));
  const f = v / 10 ** exp;
  const nf = f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10;
  return nf * 10 ** exp;
}

/** Linear-interpolated quantile (q∈0–1) over a SORTED ascending array.
 *  Single shared implementation — dedupes linerate's two copies (the
 *  `percentile`/`quantile` helpers it scattered across measurement +
 *  LatencyProfile). Returns null for an empty input. */
export function quantile(sorted: number[], q: number): number | null {
  if (!sorted.length) return null;
  if (sorted.length === 1) return sorted[0];
  const pos = (sorted.length - 1) * Math.min(1, Math.max(0, q));
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  const w = pos - lo;
  return sorted[lo] * (1 - w) + sorted[hi] * w;
}

/** "Nice" step (1/2/5 × 10ⁿ) at-or-below `span` — the rung size for a
 *  centered, snapped axis domain. Mirrors linerate's `latencyDomainStep`
 *  but generalized to any magnitude instead of the hard-coded 10/5/2/1. */
export function niceStep(span: number): number {
  if (span <= 0) return 1;
  const exp = Math.floor(Math.log10(span));
  const base = 10 ** exp;
  const f = span / base; // 1–10
  const nf = f >= 5 ? 5 : f >= 2 ? 2 : 1;
  return nf * base;
}

export interface NiceDomain {
  min: number;
  max: number;
  span: number;
}

/** Smart centered, snapped domain for an auto-scaled axis (latency-style).
 *  Lifts linerate's weighted-span math: widen the raw min–max by 1.35×
 *  (and never less than 16% of the peak, nor a small absolute floor), center
 *  it, then snap the bounds out to a nice step. Used by both the chart's
 *  latency axis and the latency profile so they scale identically. */
export function niceDomain(
  values: number[],
  opts: { widen?: number; minSpanRatio?: number; floor?: number; clampMinZero?: boolean } = {},
): NiceDomain {
  const { widen = 1.35, minSpanRatio = 0.16, floor = 12, clampMinZero = true } = opts;
  if (!values.length) return { min: 0, max: floor, span: floor };
  const rawMin = Math.min(...values);
  const rawMax = Math.max(...values);
  // No hard 1-unit floor here: on a fast LAN/localhost the whole spread is
  // sub-millisecond, and a `Math.max(1, …)` pinned the span (and thus the step)
  // at ≥1 ms regardless of the `floor` knob. The real minimum span is governed by
  // `floor` below, so callers control how small the axis can scale.
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

/** Count-up tween generator for result snap (rAF-driven, 220ms ease-out). */
export function countUp(
  from: number,
  to: number,
  durMs: number,
  onTick: (v: number) => void,
): () => void {
  const start = performance.now();
  let raf = 0;
  const ease = (t: number) => 1 - Math.pow(1 - t, 3); // matches --ease-out feel
  const loop = (now: number) => {
    const t = Math.min(1, (now - start) / durMs);
    onTick(from + (to - from) * ease(t));
    if (t < 1) raf = requestAnimationFrame(loop);
  };
  raf = requestAnimationFrame(loop);
  return () => cancelAnimationFrame(raf);
}
