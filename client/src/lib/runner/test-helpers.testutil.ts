import { DEFAULT_CONFIG } from "../state/defaults";
import type { FetchThroughputTarget, LatencyTarget } from "../api/endpoints";
import type { PreparedPaths, RunResult, RunnerConfig } from "./contract";
import type { ParticipantHost } from "./transport";
import { classifyTransportDiscovery } from "./paths";
import type { ServerCatalog } from "../servers/catalog";
import type { ConnectionPreparation } from "./real/prepare";

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
  __GM_BUILD_PROFILE__: "test",
  __GM_RELEASE_VERSION__: null,
  __GM_SOURCE_REVISION__: "test-revision",
  __GM_BUILD_IDENTITY__: "test test-revision",
  __GM_CLIENT_VERSION__: "0.0.0-test",
} as const;
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
});
/** A participant host that ignores every report unless overridden. */
export function testParticipantHost(
  config: RunnerConfig,
  overrides: Partial<ParticipantHost> = {},
): ParticipantHost {
  const ignore = () => {};
  return {
    config,
    now: () => performance.now(),
    download: ignore,
    receiver: ignore,
    latency: ignore,
    latencyInterrupted: ignore,
    latencyIncomplete: ignore,
    stall: ignore,
    resume: ignore,
    stallLatency: ignore,
    resumeLatency: ignore,
    fail: ignore,
    authenticationRequired: ignore,
    uploadHint: ignore,
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
  config.adaptive = false;
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

/** A completed single-server run result with no measurements. */
export function testRunResult(overrides: Partial<RunResult> = {}): RunResult {
  const server = { id: "self", name: "Test server", url: "http://meter.test" };
  return {
    download: null,
    upload: null,
    bidirectional: null,
    latency: null,
    latencyByStage: {
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    },
    bufferbloat: null,
    multiServer: {
      selection: [server],
      participants: [server.id],
      latencyFocus: server.id,
      servers: [],
      intervals: [],
      omittedIntervals: 0,
      failures: [],
    },
    outcome: "complete",
    stages: {
      latency: "not-run",
      download: "not-run",
      upload: "not-run",
      bidirectional: "not-run",
    },
    startedAt: 0,
    durationMs: 0,
    ...overrides,
  };
}

export function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}
export async function until(done: () => boolean, turns = 100) {
  for (let turn = 0; turn < turns && !done(); turn++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}
export const settle = () => until(() => false, 10);
/** Discovery and both verified paths of one server generation. */
export function testEvidence(generation = "gen-a"): PreparedPaths {
  const paths = testPreparedPaths();
  paths.discovery.generation = generation;
  paths.throughput.generation = generation;
  paths.latency!.generation = generation;
  return paths;
}
export function testPreparation(
  config: RunnerConfig = DEFAULT_CONFIG,
  paths = testEvidence(),
): ConnectionPreparation {
  const role = <P>(selection: string, path: P) => ({
    selection,
    state: "verified" as const,
    path,
  });
  return {
    discovery: paths.discovery,
    validation: {
      throughput: role(config.transports.throughputTarget, paths.throughput),
      latency: role(config.transports.latencyTarget, paths.latency),
    },
  };
}
