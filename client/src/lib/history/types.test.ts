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
  packetLossPct: 1,
  serverAuthoritative: true,
};
const latency = {
  idleMs: 12,
  minMs: 10,
  p50Ms: 12,
  p95Ms: 20,
  jitterMs: 2,
  packetLossPct: 0,
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
      latencyLanes: {
        download: {
          min: 11,
          max: 30,
          p10: 12,
          p90: 25,
          center: 18,
          jitter: 3,
          lossRatio: 0.1,
          count: 4,
        },
      },
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
    { ...valid, totalBytes: valid.totalBytes + 1 },
    {
      ...valid,
      stages: {
        ...valid.stages,
        download: {
          ...valid.stages.download,
          result: { ...valid.stages.download.result!, packetLossPct: 101 },
        },
      },
    },
  ];
  for (const value of cases) expect(isHistoryRecord(value)).toBe(false);
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
