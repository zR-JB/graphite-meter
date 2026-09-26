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
  adaptive: true,
  visualization: { throughputMaxBytesPerSec: "auto" },
};

type DurationKey = keyof RunnerConfig["duration"];
/** Bounds in ms, as in the native client; a stage leaves room for the 800 ms evidence floor. */
export const DURATION_LIMITS: Record<DurationKey, readonly [number, number]> = {
  warmupMs: [0, 4_000],
  latencyMs: [1_000, 300_000],
  downloadMs: [1_000, 300_000],
  uploadMs: [1_000, 300_000],
  bidirectionalMs: [1_000, 300_000],
};

export function clampDuration(key: DurationKey, value: unknown): number {
  const [min, max] = DURATION_LIMITS[key];
  if (typeof value !== "number" || !Number.isFinite(value))
    return DEFAULT_CONFIG.duration[key];
  return value <= 0 ? 0 : Math.min(max, Math.max(min, Math.round(value)));
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
