/* ============================================================
 * Gauge sweep math — the phase → 0-1 dial fraction mapping and the
 * fraction → arc-angle conversion behind GaugeEngine's #step/#draw. Pure so
 * it's unit-testable without a canvas.
 * ============================================================ */

import type { Phase } from "../runner/contract";

export function clamp01(v: number): number {
  return Math.min(1, Math.max(0, v));
}

export interface SweepTargetInput {
  phase: Phase;
  /** Current raw throughput (bytes/sec) during a transfer phase. */
  valueBytesPerSec: number;
  /** Absolute throughput scale (bytes/sec); <=0 is treated as 1 (no scale yet). */
  scaleBytesPerSec: number;
  /** Full-scale ms for the latency phase; <=0 is treated as 1. */
  latencyScaleMs: number;
  /** Current RTT (ms) during the latency phase. */
  rtt: number;
  /** Where the dial was when `complete` was entered. */
  frozenFraction: number;
}

/** The 0-1 sweep fraction the dial eases toward for the given frame's state.
 *  Mirrors GaugeEngine's per-phase target: a transfer phase reads the
 *  absolute value/scale ratio, latency reads RTT/latencyScale, warmup/idle/
 *  aborted/error hold fixed indeterminate positions, and complete holds where
 *  the live dial ended. */
export function sweepTarget(s: SweepTargetInput): number {
  switch (s.phase) {
    case "download":
    case "upload":
    case "bidirectional": {
      const scale = s.scaleBytesPerSec > 0 ? s.scaleBytesPerSec : 1;
      return clamp01(s.valueBytesPerSec / scale);
    }
    case "warmup":
      return 0.3; // indeterminate — connection probe, no meaningful rate yet
    case "latency": {
      const scale = s.latencyScaleMs > 0 ? s.latencyScaleMs : 1;
      return clamp01(s.rtt / scale);
    }
    case "idle":
      return 0.1;
    case "complete":
      return s.frozenFraction;
    default:
      return 0.05; // aborted / error
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
