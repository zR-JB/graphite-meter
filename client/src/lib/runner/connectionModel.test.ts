import { expect, test } from "bun:test";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/endpoints";
import type { InfraInfo, RunnerConfig } from "./contract";
import {
  connectionKey,
  connectionRoleKey,
  validationRoles,
  presentConnections,
  type ConnectionValidation,
} from "./connectionModel";
import { classifyTransportDiscovery } from "./real/backendPure";

const throughput: FetchThroughputTarget = {
  id: "http2",
  origin: "https://meter.test",
  transport: "fetch-stream",
  protocol: "http2",
  tls: true,
  routes: {
    probe: "/probe",
    download: "/download",
    upload: "/upload",
    uploadSession: "/upload/session",
    uploadProgress: "/upload/progress",
  },
};
const latency: WebSocketLatencyTarget = {
  id: "ws-http1-tls",
  origin: "https://meter.test:7247",
  transport: "websocket",
  protocol: "http1",
  tls: true,
  routes: { probe: "/probe", ping: "/ws/ping" },
};

function config(): RunnerConfig {
  return {
    stages: {
      latency: true,
      download: true,
      upload: true,
      bidirectional: false,
    },
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
    transferStreams: { mode: "auto", count: 6 },
    experimentalChunkedDownload: false,
    transports: { throughputTarget: "auto", latencyTarget: "auto" },
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
      glideMs: 1100,
    },
    visualization: { throughputMaxBytesPerSec: "auto" },
  };
}

function fixture() {
  return Object.assign(
    classifyTransportDiscovery(
      [throughput],
      [latency],
      "https://meter.test",
      true,
      "h2",
    ),
    {
      generation: "generation-a",
      engineVersion: "test",
      server: { name: "meter" },
      fetchedAt: 1,
    },
  );
}

test("presentation keeps browser and server protocol boundaries distinct", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  cfg.transports.latencyTarget = latency.origin;
  const validation: ConnectionValidation = {
    throughput: {
      selection: throughput.origin,
      state: "verified",
      verifiedAt: 2,
    },
    latency: { selection: latency.origin, state: "verified", verifiedAt: 2 },
  };
  const infra: InfraInfo = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4,
    clientIpSource: "forwarded",
    server: fixture().server,
    preTestPingMs: 4,
    engineVersion: "test",
    discoveryGeneration: "generation-a",
    protocolNegotiated: "http/1.1",
    firstHopProtocol: "h2",
    latencyProtocolNegotiated: "http/1.1",
  };

  const model = presentConnections(cfg, fixture(), validation, infra);

  expect(model.throughput.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(model.throughput.browserProtocol).toBe("h2");
  expect(model.throughput.serverProtocol).toBe("http/1.1");
  expect(model.latency.summary).toBe("WebSocket · HTTP/1.1 · TLS");
  expect(model.latency.preTestPingMs).toBe(4);
});

test("old evidence never appears under a new selection or generation", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  const validation: ConnectionValidation = {
    throughput: { selection: "http1-clear", state: "verified", verifiedAt: 2 },
    latency: { selection: "auto", state: "stale" },
  };
  const infra = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4 as const,
    clientIpSource: "socket" as const,
    server: fixture().server,
    preTestPingMs: 4,
    engineVersion: "test",
    discoveryGeneration: "old",
    protocolNegotiated: "h2",
  };

  expect(
    presentConnections(cfg, fixture(), validation, infra).throughput
      .serverProtocol,
  ).toBeUndefined();
  expect(
    presentConnections(cfg, fixture(), validation, infra).latency.preTestPingMs,
  ).toBeUndefined();
});

test("connection cache key changes only for preparation inputs", () => {
  const a = config();
  const b = config();
  b.visualization.throughputMaxBytesPerSec = 1_000_000;
  expect(connectionKey(a)).toBe(connectionKey(b));
  b.transports.latencyTarget = "http://meter.test";
  expect(connectionKey(a)).not.toBe(connectionKey(b));
});

test("role cache keys isolate throughput from latency preparation", () => {
  const a = config();
  const b = config();
  b.transports.throughputTarget = throughput.origin;
  expect(connectionRoleKey(a, "latency")).toBe(connectionRoleKey(b, "latency"));
  expect(connectionRoleKey(a, "throughput")).not.toBe(
    connectionRoleKey(b, "throughput"),
  );
});

test("automatic and explicit selections share an identity when they resolve to the same target", () => {
  const automatic = config();
  const explicit = config();
  explicit.transports.throughputTarget = throughput.origin;
  explicit.transports.latencyTarget = latency.origin;
  const discovery = fixture();

  expect(connectionRoleKey(automatic, "throughput", discovery)).toBe(
    connectionRoleKey(explicit, "throughput", discovery),
  );
  expect(connectionRoleKey(automatic, "latency", discovery)).toBe(
    connectionRoleKey(explicit, "latency", discovery),
  );
  expect(connectionKey(automatic, discovery)).toBe(
    connectionKey(explicit, discovery),
  );
});

test("probe failure and stale evidence remain retryable presentation states", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  const failed: ConnectionValidation = {
    throughput: {
      selection: throughput.origin,
      state: "failed",
      message: "probe timed out",
    },
    latency: { selection: "auto", state: "stale" },
  };

  const model = presentConnections(cfg, fixture(), failed, null);
  expect(model.throughput.availability).toBe("advertised");
  expect(model.throughput.validation).toBe("failed");
  expect(model.throughput.message).toBe("probe timed out");
  expect(model.latency.validation).toBe("stale");
});

test("validation retries only the changed role and carries an aborted stale role", () => {
  const cfg = config();
  const validation: ConnectionValidation = {
    throughput: { selection: "auto", state: "verified" },
    latency: { selection: "auto", state: "verified" },
  };
  cfg.transports.throughputTarget = throughput.origin;
  expect(validationRoles(cfg, validation, "throughput")).toEqual([
    "throughput",
  ]);

  validation.throughput = { selection: throughput.origin, state: "stale" };
  cfg.transports.latencyTarget = latency.origin;
  expect(validationRoles(cfg, validation, "latency")).toEqual([
    "latency",
    "throughput",
  ]);
});
