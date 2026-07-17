import { expect, test } from "bun:test";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/preflight";
import type { InfraInfo, RunnerConfig } from "./contract";
import {
  connectionKey,
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
    pingCadence: "instant",
    loadedPingCadence: "medium",
    transferStreams: { mode: "auto", count: 6 },
    experimentalChunkedDownload: false,
    transports: { throughputTarget: "current", latencyTarget: "auto" },
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
      server: { name: "meter", host: "meter.test", port: 443 },
      fetchedAt: 1,
    },
  );
}

test("presentation keeps browser and server protocol boundaries distinct", () => {
  const cfg = config();
  cfg.transports.throughputTarget = "http2";
  cfg.transports.latencyTarget = "ws-http1-tls";
  const validation: ConnectionValidation = {
    throughput: { selection: "http2", state: "verified", verifiedAt: 2 },
    latency: { selection: "ws-http1-tls", state: "verified", verifiedAt: 2 },
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
    verifiedLatencyProtocol: "http/1.1",
  };

  const model = presentConnections(cfg, fixture(), validation, infra);

  expect(model.throughput.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(model.throughput.browserProtocol).toBe("h2");
  expect(model.throughput.serverProtocol).toBe("http/1.1");
  expect(model.latency.summary).toBe("WebSocket · HTTP/1.1 · TLS");
});

test("old evidence never appears under a new selection or generation", () => {
  const cfg = config();
  cfg.transports.throughputTarget = "http2";
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
});

test("connection cache key changes only for preparation inputs", () => {
  const a = config();
  const b = config();
  b.visualization.throughputMaxBytesPerSec = 1_000_000;
  expect(connectionKey(a)).toBe(connectionKey(b));
  b.transports.latencyTarget = "ws-http1-clear";
  expect(connectionKey(a)).not.toBe(connectionKey(b));
});
