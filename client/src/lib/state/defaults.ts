import type { RunnerConfig } from "../runner/contract";

export const DEFAULT_CONFIG: RunnerConfig = {
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: {
    warmupMs: 800,
    latencyMs: 4000,
    downloadMs: 10000,
    uploadMs: 10000,
    bidirectionalMs: 10000,
  },
  pingCadence: "reply-driven",
  loadedPingCadence: "medium",
  transferStreams: { mode: "auto", count: 4 },
  experimentalDatagramThroughput: false,
  transports: {
    throughputTarget: "auto",
    latencyTarget: "auto",
  },
  adaptive: {
    enabled: true,
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
    confirmationMs: 1100,
  },
  visualization: { throughputMaxBytesPerSec: "auto" },
};

/** Adaptive thresholds are runner policy; persistence owns only the switch. */
export function canonicalAdaptiveConfig(
  enabled: unknown = DEFAULT_CONFIG.adaptive.enabled,
): RunnerConfig["adaptive"] {
  return {
    ...DEFAULT_CONFIG.adaptive,
    enabled:
      enabled === undefined
        ? DEFAULT_CONFIG.adaptive.enabled
        : enabled === true,
  };
}

export const DURATION_PRESETS = {
  short: {
    warmupMs: 600,
    latencyMs: 2500,
    downloadMs: 5000,
    uploadMs: 5000,
    bidirectionalMs: 5000,
  },
  medium: {
    warmupMs: 800,
    latencyMs: 4000,
    downloadMs: 10000,
    uploadMs: 10000,
    bidirectionalMs: 10000,
  },
  long: {
    warmupMs: 1200,
    latencyMs: 6000,
    downloadMs: 20000,
    uploadMs: 20000,
    bidirectionalMs: 20000,
  },
} as const;
