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

export function fmtSpeed(v: number): string {
  if (v >= 1000) return v.toFixed(0);
  if (v >= 100) return v.toFixed(1);
  return v.toFixed(2);
}

export function fmtMs(ms: number): string {
  return ms < 100 ? ms.toFixed(1) : ms.toFixed(0);
}

export function fmtBytes(b: number, base: "base10" | "base2"): string {
  const k = base === "base10" ? 1000 : 1024;
  const u =
    base === "base10"
      ? ["B", "kB", "MB", "GB", "TB"]
      : ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  let n = b;
  while (n >= k && i < u.length - 1) {
    n /= k;
    i++;
  }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}

export type UnitBase = "base10" | "base2";
export type UnitKind = "bits" | "bytes";

const SI_PREFIX = ["", "k", "M", "G", "T"];
const IEC_PREFIX = ["", "Ki", "Mi", "Gi", "Ti"];

export function rateUnit(base: UnitBase, kind: UnitKind, idx: number): string {
  const prefixes = base === "base10" ? SI_PREFIX : IEC_PREFIX;
  const p = prefixes[Math.max(0, Math.min(prefixes.length - 1, idx))];
  return kind === "bits" ? `${p}bit/s` : `${p}B/s`;
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
  // `headroom` delays prefix promotion so values near 1000 do not flip between
  // e.g. 999 Mbit/s and 1.00 Gbit/s as samples jitter around the boundary.
  const k = base === "base10" ? 1000 : 1024;
  if (baseUnits < headroom) return 0;
  return Math.max(
    0,
    Math.min(4, Math.floor(Math.log(baseUnits / headroom) / Math.log(k))),
  );
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

export function sharedThroughputScale(peakBytesPerSec: number): number {
  // Throughput scale is always chosen in bit/s, then converted back to bytes/s,
  // so bits and bytes displays share the same visual ceiling.
  if (peakBytesPerSec <= 0) return 1.25e7;
  return ceil125(peakBytesPerSec * 8) / 8;
}

export function niceCeil(v: number): number {
  if (v <= 0) return 1;
  return ceil125(v);
}

export function quantile(sorted: number[], q: number): number | null {
  if (!sorted.length) return null;
  if (sorted.length === 1) return sorted[0];
  const pos = (sorted.length - 1) * Math.min(1, Math.max(0, q));
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  const w = pos - lo;
  return sorted[lo] * (1 - w) + sorted[hi] * w;
}

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

export function niceDomain(
  values: number[],
  opts: {
    widen?: number;
    minSpanRatio?: number;
    floor?: number;
    clampMinZero?: boolean;
  } = {},
): NiceDomain {
  // Widen around the observed range but enforce a minimum span so flat series
  // still render as readable charts instead of a line glued to an edge.
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

function ceil125(v: number): number {
  const exp = Math.floor(Math.log10(v));
  const base = 10 ** exp;
  const f = v / base;
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * base;
}
