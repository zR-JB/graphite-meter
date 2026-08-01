// The shipped run configuration and its duration presets. Separate from the
// store so persistence can read the defaults without importing it back.
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
  // The automatic H1 ceiling, and the starting point for a forced count.
  // 2 lanes is the loopback optimum, beating 4 by 9.6%; 4 is the plateau under
  // loss, so it ships as the compromise. See docs/BENCHMARKS.md.
  transferStreams: { mode: "auto", count: 4 },
  experimentalChunkedDownload: false,
  experimentalDatagramThroughput: false,
  transports: {
    throughputTarget: "auto",
    latencyTarget: "auto",
  },
  compensation: {
    profile: "lan",
    transport: "auto",
    params: {
      mtuBytes: 1500,
      ipVersion: "auto",
      vlanTagged: false,
      tcpOptionsMinBytes: 0,
      tcpOptionsMaxBytes: 12,
      encapsulationBytes: 0,
      quicConnIdMinBytes: 0,
      quicConnIdMaxBytes: 20,
    },
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
