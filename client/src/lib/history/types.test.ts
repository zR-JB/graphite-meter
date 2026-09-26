import { expect, test } from "bun:test";
import { buildHistoryRecord, isHistoryRecord } from "./types";
import type { RunResult } from "../runner/contract";
import { testPreparedPaths } from "../runner/test-helpers.testutil";

const throughput = {
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

function serverHistoryRecord() {
  const source = structuredClone(result);
  const server = { id: "a", name: "A", url: "https://a.example" };
  source.multiServer = {
    selection: [server],
    participants: [server.id],
    latencyFocus: server.id,
    intervals: [],
    omittedIntervals: 0,
    failures: [],
    servers: [
      {
        server,
        throughput: {
          origin: server.url,
          transport: "fetch-stream",
          protocol: "http2",
        },
        latencyTarget: { origin: server.url, transport: "websocket" },
        latency: source.latency,
        latencyByStage: source.latencyByStage,
        bufferbloat: source.bufferbloat,
        download: source.download,
        upload: source.upload,
        bidirectional: source.bidirectional,
        totalBytes: { down: 800, up: 0 },
      },
    ],
  };
  return buildHistoryRecord(source, { paths: null, clientBuild: "b" }, 200);
}

test("builds an immutable sanitized partial snapshot", () => {
  const paths = testPreparedPaths();
  paths.discovery.server = { name: "edge", location: "EU" };
  paths.discovery.engineVersion = "e";
  paths.throughput.probe.clientIp = "192.0.2.5";
  paths.throughput.probe.clientIpVersion = 4;
  paths.throughput.probe.protocolNegotiated = "h2";
  paths.throughput.target.id = "https://secret.invalid/raw";
  paths.latency!.probe.protocolNegotiated = "http/1.1";
  // The persistence boundary rejects a runtime-invalid latency mechanism.
  (paths.latency!.target as { transport: string }).transport =
    "webtransport-datagram";
  const record = buildHistoryRecord(
    result,
    {
      paths,
      clientBuild: "b",
      wireEstimates: {
        version: 2,
        breakdown: { download: null, upload: null, bidirectional: null },
        downloadBytesPerSec: 101,
        uploadBytesPerSec: null,
        bidirectionalBytesPerSec: 102,
      },
    },
    200,
  );
  paths.discovery.server.name = "changed after completion";
  paths.throughput.probe.protocolNegotiated = "h3";
  expect(record.server).toEqual({ name: "edge", location: "EU", engine: "e" });
  expect(record.transport.throughput.protocol).toBe("h2");
  expect(record.completedAt).toBe(200);
  expect(record.stages.download.result).toMatchObject({
    reportedBytesPerSec: 100,
    fullAverageBytesPerSec: 90,
    peakBytesPerSec: 120,
  });
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
  expect(isHistoryRecord({ ...record, id: 5 })).toBe(false);
});

test("round-trips current records, including per-server details, and rejects other versions", () => {
  const record = buildHistoryRecord(
    result,
    { paths: null, clientBuild: "b" },
    200,
  );
  const reloaded = JSON.parse(JSON.stringify(record));
  expect(isHistoryRecord(reloaded)).toBe(true);
  expect(reloaded).toEqual(record);
  expect(isHistoryRecord(serverHistoryRecord())).toBe(true);
  for (const schemaVersion of [undefined, 1, 2, 3, 5])
    expect(isHistoryRecord({ ...record, schemaVersion })).toBe(false);
});

test("corrupted saved shapes are skipped before they reach rendering", () => {
  const valid = serverHistoryRecord();
  const lanes = valid.stages.latency.lanes;
  const cases: unknown[] = [
    { ...valid, stages: null },
    { ...valid, failures: {} },
    { ...valid, durationMs: "1" },
    {
      ...valid,
      stages: {
        ...valid.stages,
        latency: {
          ...valid.stages.latency,
          lanes: { ...lanes, download: { ...lanes.download, center: NaN } },
        },
      },
    },
    {
      ...valid,
      stages: {
        ...valid.stages,
        download: { status: "complete", result: { reportedBytesPerSec: 1 } },
      },
    },
    { ...valid, wireEstimates: { downloadBytesPerSec: Infinity } },
    { ...valid, multiServer: { ...valid.multiServer, servers: [null] } },
    { ...valid, server: { name: "x".repeat(4096) } },
    { ...valid, failures: Array.from({ length: 600 }, () => null) },
  ];
  for (const value of cases) expect(isHistoryRecord(value)).toBe(false);
});

test("record construction bounds persisted display text without losing the run", () => {
  const long = "x".repeat(300);
  const paths = testPreparedPaths();
  paths.discovery.server = { name: long, location: long };
  paths.discovery.engineVersion = long;
  (
    paths.throughput.probe as { protocolNegotiated: string }
  ).protocolNegotiated = long;
  (paths.latency!.probe as { protocolNegotiated: string }).protocolNegotiated =
    "https://secret.invalid/raw";
  const record = buildHistoryRecord(
    result,
    {
      paths,
      clientBuild: long,
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

test("current history persists partial accounting and exact known outcome counts", () => {
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
    { paths: null, clientBuild: "b" },
    200,
  );
  expect(saved.stages.latency.lanes.download).toMatchObject({
    accountingComplete: false,
    count: 3,
    timeoutCount: 1,
    timeoutRatio: 1 / 3,
    unresolvedCount: 2,
    sendFailureCount: 4,
  });
  expect(isHistoryRecord(JSON.parse(JSON.stringify(saved)))).toBe(true);
});

test("optional paired server timing is copied without changing saved raw methodology", () => {
  const source = structuredClone(result);
  source.latencyByStage.download!.reflectorTiming = {
    sampleCount: 2,
    meanRawRttMs: 18,
    meanHandlingMs: 3,
    meanAdjustedRttMs: 15,
  };
  const saved = buildHistoryRecord(
    source,
    { paths: null, clientBuild: "b" },
    200,
  );
  const lane = saved.stages.latency.lanes.download!;
  expect(isHistoryRecord(saved)).toBe(true);
  expect(lane.reflectorTiming).toEqual(
    source.latencyByStage.download!.reflectorTiming!,
  );
  expect(lane.center).toBe(18);
  expect(lane.min).toBe(11);
  source.latencyByStage.download!.reflectorTiming!.meanHandlingMs = 99;
  expect(lane.reflectorTiming!.meanHandlingMs).toBe(3);
  delete lane.reflectorTiming;
  expect(isHistoryRecord(saved)).toBe(true);
});
