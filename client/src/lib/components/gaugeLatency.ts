import type { LatencyBucket, Phase } from "../runner/contract";
import {
  latencyScaleForHistory,
  latencyScaleForReading,
} from "../runner/latencyScale";

export interface GaugeLatencyPresentation {
  rttMs: number;
  scaleMs: number;
}

export interface GaugeLatencyState {
  phase: Phase;
  liveRttMs: number;
  liveScaleMs: number;
  history: readonly LatencyBucket[];
  completedRttMs: number | null;
}

/** Resolve the gauge's latency value and domain as one presentation decision.
 * Live buckets use the shared recent controller. A pre-bucket fallback derives
 * a domain from that fallback, while a terminal value uses the same full-run
 * history as the terminal chart. */
export function gaugeLatencyPresentation(
  state: GaugeLatencyState,
): GaugeLatencyPresentation {
  if (state.phase === "complete" && state.completedRttMs != null) {
    const hasMeasuredMedian = state.history.some(
      (bucket) => bucket.medianRttMs != null,
    );
    return {
      rttMs: state.completedRttMs,
      scaleMs: hasMeasuredMedian
        ? latencyScaleForHistory(state.history)
        : latencyScaleForReading(state.completedRttMs),
    };
  }

  const hasMeasuredLatencyBucket =
    state.phase === "latency" && state.history.at(-1)?.phase === "latency";
  return {
    rttMs: state.liveRttMs,
    scaleMs: hasMeasuredLatencyBucket
      ? state.liveScaleMs
      : latencyScaleForReading(state.liveRttMs),
  };
}
