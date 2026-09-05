import { expect, test } from "bun:test";
import { buildHistoryRecord, isHistoryRecord } from "./types";
import type { RunResult } from "../runner/contract";

const throughput = {
  meanBytesPerSec: 100,
  peakBytesPerSec: 120,
  stabilityPct: 3,
  totalBytes: 400,
  reportedBytesPerSec: 100,
  fullAverageBytesPerSec: 90,
  method: "full-average" as const,
  stabilityScore: 0.9,
  band: "high" as const,
  probeTimeoutPct: 1,
  serverAuthoritative: true,
};
const latency = {
  idleMs: 12,
  minMs: 10,
  p50Ms: 12,
  p95Ms: 20,
  jitterMs: 2,
  probeTimeoutPct: 0,
  reportedMs: 12,
  method: "full-average" as const,
  stabilityScore: 1,
  band: "high" as const,
};
const result: RunResult = {
  download: throughput,
  upload: null,
  bidirectional: { down: throughput, up: null },
  latency,
  latencyByStage: {
    latency: null,
    upload: null,
    bidirectional: null,
    download: {
      accountingComplete: true,
      minMs: 11,
      maxMs: 30,
      p10Ms: 12,
      p50Ms: 18,
      p90Ms: 25,
      p95Ms: 30,
      meanMs: 18,
      jitterMs: 3,
      unresolvedCount: 0,
      sendFailureCount: 0,
      jitterPairs: 8,
      probeCount: 10,
      timeoutCount: 1,
    },
  },
  bufferbloat: { grade: "B", idleMs: 12, loadedMs: 20, increaseMs: 8 },
  stageFailures: {
    upload: { stage: "upload", reason: "timeout", message: "raw secret" },
  },
  startedAt: 100,
  durationMs: 80,
};
test("builds an immutable sanitized partial snapshot", () => {
  const record = buildHistoryRecord(
    result,
    {
      infra: {
        clientIp: "192.0.2.5",
        clientIpVersion: 4,
        clientIpSource: "socket",
        server: { name: "edge", location: "EU" },
        preTestPingMs: 4,
        engineVersion: "e",
        discoveryGeneration: "g",
        protocolNegotiated: "h2",
        selectedThroughputTarget: "https://secret.invalid/raw",
        selectedThroughputTransport: "webtransport",
        selectedLatencyTransport: "webtransport-datagram",
        latencyProtocolNegotiated: "h1",
      },
      clientBuild: "b",
      engineVersion: "e",
      wireDownloadBytesPerSec: 101,
      wireBidirectionalBytesPerSec: 102,
    },
    200,
  );
  expect(record.completedAt).toBe(200);
  expect(record.stages.download.result?.peakBytesPerSec).toBe(120);
  expect(record.stages.bidirectional.status).toBe("partial");
  expect(record.wireEstimates?.downloadBytesPerSec).toBe(101);
  expect(record.wireEstimates?.uploadBytesPerSec).toBeNull();
  expect(record.wireEstimates?.bidirectionalBytesPerSec).toBe(102);
  expect(record.stages.latency.lanes.download?.center).toBe(18);
  expect(record.totalBytes).toBe(800);
  expect(record.ipVersion).toBe(4);
  expect(record.transport.latency.kind).toBeNull();
  expect(JSON.stringify(record)).not.toContain("secret.invalid");
  expect(JSON.stringify(record)).not.toContain("raw secret");
  expect(JSON.stringify(record)).not.toContain("192.0.2.5");
  expect(isHistoryRecord(record)).toBe(true);
  expect(isHistoryRecord({ ...record, id: "bad" })).toBe(false);
});

test("V1 records remain readable while V2 fields cannot be mislabeled as legacy", () => {
  const record = buildHistoryRecord(
    result,
    { infra: null, clientBuild: "b", engineVersion: "e" },
    200,
  );
  expect(record.schemaVersion).toBe(2);
  expect(isHistoryRecord({ ...record, schemaVersion: 1 })).toBe(false);
  const legacy = structuredClone(record);
  legacy.schemaVersion = 1;
  for (const snapshot of [
    legacy.stages.download.result,
    legacy.stages.upload.result,
    legacy.stages.bidirectional.down,
    legacy.stages.bidirectional.up,
    legacy.stages.latency.result,
  ]) {
    if (!snapshot) continue;
    snapshot.packetLossPct = snapshot.probeTimeoutPct ?? 0;
    delete snapshot.probeTimeoutPct;
  }
  for (const lane of Object.values(legacy.stages.latency.lanes)) {
    if (!lane) continue;
    lane.lossRatio = lane.timeoutRatio ?? 0;
    delete lane.timeoutRatio;
    delete lane.accountingComplete;
    delete lane.timeoutCount;
    delete lane.unresolvedCount;
    delete lane.sendFailureCount;
  }
  expect(isHistoryRecord(legacy)).toBe(true);
  expect(isHistoryRecord({ ...legacy, schemaVersion: 2 })).toBe(false);
});

test("rejects malformed nested records before they reach rendering", () => {
  const valid = buildHistoryRecord(
    result,
    { infra: null, clientBuild: "b", engineVersion: "e" },
    200,
  );
  const cases: unknown[] = [
    { ...valid, stages: null },
    {
      ...valid,
      stages: {
        ...valid.stages,
        latency: {
          ...valid.stages.latency,
          lanes: { ...valid.stages.latency.lanes, download: { center: NaN } },
        },
      },
    },
    {
      ...valid,
      transport: {
        ...valid.transport,
        throughput: { protocol: "https://raw", kind: null },
      },
    },
    {
      ...valid,
      transport: {
        ...valid.transport,
        latency: { protocol: "h3", kind: "webtransport-datagram" },
      },
    },
    {
      ...valid,
      transport: {
        ...valid.transport,
        throughput: { protocol: "h1", kind: "websocket" },
      },
    },
    {
      ...valid,
      wireEstimates: {
        version: 1,
        downloadBytesPerSec: Infinity,
        uploadBytesPerSec: null,
        bidirectionalBytesPerSec: null,
      },
    },
    {
      ...valid,
      failures: [
        { stage: "download", direction: "sideways", reason: "internal-error" },
      ],
    },
    { ...valid, bufferbloat: { ...valid.bufferbloat, grade: "Z" } },
    { ...valid, durationMs: -1 },
    { ...valid, failures: [valid.failures[0], valid.failures[0]] },
    { ...valid, failures: Array.from({ length: 5 }, () => valid.failures[0]) },
    { ...valid, totalBytes: valid.totalBytes + 1 },
    {
      ...valid,
      stages: {
        ...valid.stages,
        download: {
          ...valid.stages.download,
          result: { ...valid.stages.download.result!, probeTimeoutPct: 101 },
        },
      },
    },
  ];
  for (const value of cases) expect(isHistoryRecord(value)).toBe(false);
});

test("authoritative scalar counts are not rejected by an unrelated ceiling", () => {
  const record = buildHistoryRecord(
    {
      ...result,
      latencyByStage: {
        ...result.latencyByStage,
        latency: {
          ...result.latencyByStage.download!,
          probeCount: Number.MAX_SAFE_INTEGER,
        },
      },
    },
    {
      infra: null,
      clientBuild: "b",
      engineVersion: "e",
    },
    200,
  );
  expect(isHistoryRecord(record)).toBe(true);
  expect(record.stages.latency.lanes.latency?.count).toBe(
    Number.MAX_SAFE_INTEGER,
  );
});

test("record construction bounds persisted display text without losing the run", () => {
  const long = "x".repeat(300);
  const record = buildHistoryRecord(
    result,
    {
      infra: {
        clientIp: "192.0.2.5",
        clientIpVersion: 4,
        clientIpSource: "socket",
        server: { name: long, location: long },
        preTestPingMs: 4,
        engineVersion: long,
        discoveryGeneration: "g",
        protocolNegotiated: long,
        selectedThroughputTransport: "fetch-stream",
        selectedLatencyTransport: "websocket",
        latencyProtocolNegotiated: "https://secret.invalid/raw",
      },
      clientBuild: long,
      engineVersion: long,
    },
    200,
  );
  expect(isHistoryRecord(record)).toBe(true);
  expect(record.server.name).toHaveLength(256);
  expect(record.server.location).toHaveLength(256);
  expect(record.server.engine).toHaveLength(256);
  expect(record.client.build).toHaveLength(256);
  expect(record.transport.throughput.protocol).toHaveLength(256);
  expect(record.transport.latency.protocol).toBeNull();
  expect(JSON.stringify(record)).not.toContain("secret.invalid");
});

test("failure snapshots are bounded by the four authoritative run stages", () => {
  const record = buildHistoryRecord(
    {
      ...result,
      stageFailures: {
        latency: { stage: "latency", reason: "timeout", message: "raw" },
        download: { stage: "download", reason: "timeout", message: "raw" },
        upload: { stage: "upload", reason: "timeout", message: "raw" },
        bidirectional: {
          stage: "bidirectional",
          reason: "timeout",
          message: "raw",
        },
      },
    },
    { infra: null, clientBuild: "b", engineVersion: "e" },
    200,
  );
  expect(record.failures).toHaveLength(4);
  expect(isHistoryRecord(record)).toBe(true);
});

test("rejects non-date epochs while retaining valid date bounds", () => {
  const valid = buildHistoryRecord(
    result,
    { infra: null, clientBuild: "b", engineVersion: "e" },
    200,
  );
  const maxDate = 8_640_000_000_000_000;
  expect(
    isHistoryRecord({ ...valid, startedAt: maxDate, completedAt: maxDate }),
  ).toBe(true);
  for (const value of [1e20, maxDate + 1, Number.MAX_SAFE_INTEGER, 1.5, -1]) {
    expect(isHistoryRecord({ ...valid, startedAt: value })).toBe(false);
    expect(isHistoryRecord({ ...valid, completedAt: value })).toBe(false);
  }
});

test("V2 persists partial accounting and exact known outcome counts", () => {
  const partial = structuredClone(result);
  partial.latencyByStage.download = {
    ...partial.latencyByStage.download!,
    accountingComplete: false,
    probeCount: 3,
    timeoutCount: 1,
    unresolvedCount: 2,
    sendFailureCount: 4,
  };
  const saved = buildHistoryRecord(
    partial,
    { infra: null, clientBuild: "b", engineVersion: "e" },
    200,
  );
  expect(saved.stages.latency.lanes.download).toMatchObject({
    accountingComplete: false,
    count: 3,
    timeoutCount: 1,
    unresolvedCount: 2,
    sendFailureCount: 4,
  });
  expect(isHistoryRecord(JSON.parse(JSON.stringify(saved)))).toBe(true);
  for (const field of ["accountingComplete", "timeoutCount"] as const) {
    const missing = structuredClone(saved);
    delete missing.stages.latency.lanes.download![field];
    expect(isHistoryRecord(missing)).toBe(false);
  }
  saved.stages.latency.lanes.download!.timeoutCount = 4;
  expect(isHistoryRecord(saved)).toBe(false);
});
