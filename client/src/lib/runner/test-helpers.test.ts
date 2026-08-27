import { DEFAULT_CONFIG } from "../state/defaults";
import type { RunnerConfig } from "./contract";

export const TEST_BUILD_TOKENS = {
  __GM_ALLOW_DUMMY__: false,
  __GM_BUILD_PROFILE__: "test",
  __GM_RELEASE_VERSION__: null,
  __GM_SOURCE_REVISION__: "test-revision",
  __GM_BUILD_IDENTITY__: "test test-revision",
  __GM_CLIENT_VERSION__: "0.0.0-test",
} as const;
export const TEST_WT_ORIGIN = "https://meter.test";
export const TEST_WT_PREFLIGHT = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [
      { baseUrl: TEST_WT_ORIGIN, transport: "webtransport", protocol: "http3" },
    ],
    latency: [{ baseUrl: TEST_WT_ORIGIN, transport: "websocket" }],
  },
};
export function testWtConfig(
  stages: RunnerConfig["stages"] = {
    latency: false,
    download: true,
    upload: true,
    bidirectional: false,
  },
): RunnerConfig {
  const config = structuredClone(DEFAULT_CONFIG);
  config.stages = stages;
  config.transports = {
    throughputTarget: `${TEST_WT_ORIGIN}::wt`,
    latencyTarget: "auto",
  };
  config.transferStreams = { mode: "forced", count: 4 };
  config.duration = {
    warmupMs: 0,
    latencyMs: 1,
    downloadMs: 1,
    uploadMs: 1,
    bidirectionalMs: 1,
  };
  config.adaptive = {
    ...config.adaptive,
    enabled: false,
    minCoverageRatio: 1,
    stabilityThreshold: 1,
    maxPhaseReductionRatio: 0,
    minLatencySamples: 1,
    minTransferSamples: 1,
    confirmationMs: 0,
  };
  return config;
}
