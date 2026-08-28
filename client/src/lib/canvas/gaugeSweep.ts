// Pure dial mapping and interpolation.
import type { Phase } from "../runner/contract";
import { throughputGaugeFraction } from "./gaugeScale";
export function clamp01(v: number): number {
  return Math.min(1, Math.max(0, v));
}
export interface SweepTargetInput {
  phase: Phase;
  /** Current raw throughput (bytes/sec) during a transfer phase. */
  valueBytesPerSec: number;
  /** Absolute throughput scale (bytes/sec); <=0 is treated as 1 (no scale yet). */
  scaleBytesPerSec: number;
  /** True only after an authoritative sample for the current transfer phase. */
  throughputEvidence: boolean;
  /** Full-scale ms for the latency phase; <=0 is treated as 1. */
  latencyScaleMs: number;
  /** Current RTT (ms) during the latency phase. */
  rtt: number;
  /** Metric represented after completion. */
  completedKind: "speed" | "latency";
}
/** Fixed positions for the phases that carry no measurable value. */
const NEUTRAL_SWEEP = 0.5;
const FAULT_SWEEP = 0.05;
/** The 0-1 sweep fraction the dial eases toward for the given frame's state. */
export function sweepTarget(s: SweepTargetInput): number {
  const throughput = () => {
    if (!s.throughputEvidence) return NEUTRAL_SWEEP;
    return throughputGaugeFraction(s.valueBytesPerSec, s.scaleBytesPerSec);
  };
  const latency = () => {
    const scale = s.latencyScaleMs > 0 ? s.latencyScaleMs : 1;
    return clamp01(s.rtt / scale);
  };
  switch (s.phase) {
    case "download":
    case "upload":
    case "bidirectional":
      return throughput();
    case "connecting":
    case "warmup":
      return NEUTRAL_SWEEP;
    case "latency":
      return latency();
    case "idle":
      return NEUTRAL_SWEEP;
    case "complete":
      return s.completedKind === "latency" ? latency() : throughput();
    case "aborted":
    case "error":
    default:
      return FAULT_SWEEP;
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
export function interpolateSweep(
  current: number,
  target: number,
  elapsedMs: number,
  reducedMotion: boolean,
): { value: number; active: boolean } {
  const delta = target - current;
  if (reducedMotion || Math.abs(delta) < 0.002)
    return { value: target, active: false };
  return {
    value: current + delta * (1 - Math.exp(-Math.min(100, elapsedMs) / 100)),
    active: true,
  };
}
