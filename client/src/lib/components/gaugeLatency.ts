import type { LatencyBucket, Phase } from "../runner/contract";
import {
  latencyScaleForHistory,
  latencyScaleForReading,
} from "../runner/latencyScale";

interface GaugeLatencyPresentation {
  rttMs: number;
  scaleMs: number;
}

interface GaugeLatencyState {
  phase: Phase;
  liveRttMs: number;
  liveScaleMs: number;
  history: readonly LatencyBucket[];
  completedRttMs: number | null;
}

/* Resolve the gauge's latency value and domain as one presentation decision. */
export function gaugeLatencyPresentation(
  state: GaugeLatencyState,
): GaugeLatencyPresentation {
  const hasMeasuredLatencyBucket =
    state.phase === "latency" && state.history.at(-1)?.phase === "latency";
  if (state.phase === "complete" && state.completedRttMs != null) {
    const scale = state.history.some((bucket) => bucket.medianRttMs != null)
      ? latencyScaleForHistory(state.history)
      : latencyScaleForReading(state.completedRttMs);
    return { rttMs: state.completedRttMs, scaleMs: scale };
  }
  return {
    rttMs: state.liveRttMs,
    scaleMs: hasMeasuredLatencyBucket
      ? state.liveScaleMs
      : latencyScaleForReading(state.liveRttMs),
  };
}
