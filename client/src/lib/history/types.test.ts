import { expect, test } from "bun:test";
import { buildHistoryRecord, isHistoryRecord } from "./types";
import type { RunResult } from "../runner/contract";
import { testPreparedPaths } from "../runner/test-helpers.test";

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
        version: 1,
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
  expect(isHistoryRecord({ ...record, id: "bad" })).toBe(false);
});

test("writes v4, preserves v3 singletons and rejects obsolete fields", () => {
  const record = buildHistoryRecord(
    result,
    { paths: null, clientBuild: "b" },
    200,
  );
  expect(record.schemaVersion).toBe(4);
  expect(isHistoryRecord({ ...record, schemaVersion: 3 })).toBe(true);
  for (const schemaVersion of [undefined, 1, 2, 5])
    expect(isHistoryRecord({ ...record, schemaVersion })).toBe(false);
  for (const obsolete of ["meanBytesPerSec", "packetLossPct"]) {
    const saved = structuredClone(record);
    Object.assign(saved.stages.download.result!, { [obsolete]: 0 });
    expect(isHistoryRecord(saved)).toBe(false);
  }
  const obsoleteLane = structuredClone(record);
  Object.assign(obsoleteLane.stages.latency.lanes.download!, { lossRatio: 0 });
  expect(isHistoryRecord(obsoleteLane)).toBe(false);
});

test("rejects malformed nested records before they reach rendering", () => {
  const valid = buildHistoryRecord(
    result,
    { paths: null, clientBuild: "b" },
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
      paths: null,
      clientBuild: "b",
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
    { paths: null, clientBuild: "b" },
    200,
  );
  expect(record.failures).toHaveLength(4);
  expect(isHistoryRecord(record)).toBe(true);
});

test("rejects non-date epochs while retaining valid date bounds", () => {
  const valid = buildHistoryRecord(
    result,
    { paths: null, clientBuild: "b" },
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
    unresolvedCount: 2,
    sendFailureCount: 4,
  });
  expect(isHistoryRecord(JSON.parse(JSON.stringify(saved)))).toBe(true);
  for (const field of [
    "accountingComplete",
    "timeoutCount",
    "timeoutRatio",
    "unresolvedCount",
    "sendFailureCount",
    "count",
  ]) {
    const missing = structuredClone(saved);
    delete (
      missing.stages.latency.lanes.download! as unknown as Record<
        string,
        unknown
      >
    )[field];
    expect(isHistoryRecord(missing)).toBe(false);
  }
  const missingMetadata = structuredClone(saved);
  const incomplete = missingMetadata.stages.latency.lanes
    .download! as unknown as Record<string, unknown>;
  delete incomplete.accountingComplete;
  delete incomplete.timeoutCount;
  expect(isHistoryRecord(missingMetadata)).toBe(false);
  saved.stages.latency.lanes.download!.timeoutCount = 4;
  expect(isHistoryRecord(saved)).toBe(false);
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

test("saved timing requires a bounded paired population and consistent finite means", () => {
  const saved = buildHistoryRecord(
    result,
    { paths: null, clientBuild: "b" },
    200,
  );
  const valid = {
    sampleCount: 2,
    meanRawRttMs: 18,
    meanHandlingMs: 3,
    meanAdjustedRttMs: 15,
  };
  const lane = saved.stages.latency.lanes.download!;
  for (const invalid of [
    null,
    { ...valid, sampleCount: 0 },
    { ...valid, sampleCount: 1.5 },
    { ...valid, sampleCount: 10 }, // Only nine resolved replies were successful.
    { ...valid, sampleCount: Number.MAX_SAFE_INTEGER + 1 },
    { ...valid, meanRawRttMs: Infinity },
    { ...valid, meanHandlingMs: -1 },
    { ...valid, meanHandlingMs: 19 },
    { ...valid, meanAdjustedRttMs: -1 },
    { ...valid, meanAdjustedRttMs: 14 },
    { ...valid, unknown: true },
  ]) {
    (lane as unknown as Record<string, unknown>).reflectorTiming = invalid;
    expect(isHistoryRecord(saved)).toBe(false);
  }
  lane.reflectorTiming = {
    sampleCount: 1,
    meanRawRttMs: 0,
    meanHandlingMs: 0,
    meanAdjustedRttMs: 0,
  };
  expect(isHistoryRecord(saved)).toBe(true);
});
