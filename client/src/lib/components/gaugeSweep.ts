// Pure measurement-to-dial mapping.
import type { Phase } from "../runner/contract";
import { throughputGaugeFraction } from "./gaugeScale";
function clamp01(v: number): number {
  return Math.min(1, Math.max(0, v));
}
export interface SweepTargetInput {
  phase: Phase;
  /** Current raw throughput (bytes/sec) during a transfer phase. */
  valueBytesPerSec: number;
  /** Absolute throughput scale (bytes/sec); <=0 is treated as 1 (no scale yet). */
  scaleBytesPerSec: number;
  /** True once the run's transfer rates have evidence; a warmup holds the last stage's. */
  throughputEvidence: boolean;
  /** Full-scale ms for the latency phase; <=0 is treated as 1. */
  latencyScaleMs: number;
  /** Current RTT (ms) during the latency phase. */
  rtt: number;
  /** Metric represented after completion. */
  completedKind: "speed" | "latency";
}
/** The 0-1 sweep for the value the dial shows, or null while it shows none. */
export function sweepTarget(s: SweepTargetInput): number | null {
  const latency = () =>
    clamp01(s.rtt / (s.latencyScaleMs > 0 ? s.latencyScaleMs : 1));
  switch (s.phase) {
    case "latency":
      return latency();
    case "warmup":
    case "download":
    case "upload":
    case "bidirectional":
      return s.throughputEvidence
        ? throughputGaugeFraction(s.valueBytesPerSec, s.scaleBytesPerSec)
        : null;
    case "complete":
      return s.completedKind === "latency" ? latency() : null;
    default:
      return null;
  }
}
/** Map a 0-1 sweep fraction to its position (radians) along the dial's arc. */
export function angleForFraction(
  fraction: number,
  arcStart: number,
  arcSweep: number,
): number {
  return arcStart + arcSweep * clamp01(fraction);
}
