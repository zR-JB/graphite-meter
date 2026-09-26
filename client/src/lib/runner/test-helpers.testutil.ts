import { DEFAULT_CONFIG } from "../state/defaults";
import type { FetchThroughputTarget, LatencyTarget } from "../api/endpoints";
import type { CoreHost } from "./core";
import type { PreparedPaths, RunnerConfig } from "./contract";
import { classifyTransportDiscovery, ROUTES } from "./real/backendPure";
import type { ServerCatalog } from "../servers/catalog";

/** A real self-server selection for controller tests with injected network operations. */
export async function testServerCatalog(): Promise<ServerCatalog> {
  return {
    servers: [{ id: "self", name: "Test server", url: location.origin }],
    defaultSelection: ["self"],
  };
}
export async function testServerDiscovery() {
  return testPreparedPaths().discovery;
}

export const TEST_BUILD_TOKENS = {
  __GM_ALLOW_DUMMY__: false,
  __GM_BUILD_PROFILE__: "test",
  __GM_RELEASE_VERSION__: null,
  __GM_SOURCE_REVISION__: "test-revision",
  __GM_BUILD_IDENTITY__: "test test-revision",
  __GM_CLIENT_VERSION__: "0.0.0-test",
} as const;
const testRoutes = {
  probe: ROUTES.probe,
  download: ROUTES.download,
  upload: ROUTES.upload,
  uploadSession: ROUTES.uploadSession,
  uploadProgress: ROUTES.uploadProgress,
};
export const testTransfer = (
  id: string,
  origin: string,
  protocol: FetchThroughputTarget["protocol"],
  tls: boolean,
): FetchThroughputTarget => ({
  id,
  origin,
  transport: "fetch-stream",
  protocol,
  tls,
  routes: testRoutes,
});
export const testLatency = (
  id: string,
  origin: string,
  tls: boolean,
): LatencyTarget => ({
  id,
  origin,
  protocol: "http1",
  tls,
  transport: "websocket",
  routes: { probe: ROUTES.probe, ping: ROUTES.ping },
});
export function testHost(
  config: RunnerConfig,
  overrides: Partial<CoreHost> = {},
): CoreHost {
  return {
    config,
    phase: "idle",
    elapsed: 0,
    emit() {},
    fail() {},
    failStage() {},
    ingestThroughput() {},
    ingestLatency() {},
    ingestLatencyInterruption() {},
    ingestLatencyAccountingIncomplete() {},
    recordRecoveryGap() {},
    recordRecoveryBytes() {},
    presentationRate: () => 0,
    stall() {},
    resume() {},
    ...overrides,
  };
}
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

/** Complete verified connection values for lifecycle and privacy boundary fixtures. */
export function testPreparedPaths(
  overrides: Partial<PreparedPaths> = {},
): PreparedPaths {
  const origin = "http://meter.test";
  const discovery = {
    ...classifyTransportDiscovery(
      [{ baseUrl: origin, transport: "fetch-stream", protocol: "http1" }],
      [{ baseUrl: origin, transport: "websocket" }],
      origin,
      false,
      "http/1.1",
    ),
    generation: "gen-a",
    uploadCheckpoint: true,
    engineVersion: "1.2.3",
    server: { name: "node-a", location: "Somewhere" },
    fetchedAt: Date.now(),
  };
  const throughput = discovery.throughput[origin]
    .targets[0] as FetchThroughputTarget;
  const latency = discovery.latency[origin].targets[0];
  const probe = {
    clientIp: "203.0.113.7",
    clientIpVersion: 4 as const,
    clientIpSource: "socket" as const,
    protocolNegotiated: "http/1.1" as const,
  };
  return {
    discovery,
    throughput: {
      requested: throughput,
      target: throughput,
      fetch: throughput,
      probe: { ...probe, load: { active: 3, max: 4 } },
      browserProtocol: "http/1.1",
      generation: discovery.generation,
      verifiedAt: Date.now(),
    },
    latency: {
      requested: latency,
      target: latency,
      probe,
      rttMs: 12,
      generation: discovery.generation,
      verifiedAt: Date.now(),
    },
    ...overrides,
  };
}
