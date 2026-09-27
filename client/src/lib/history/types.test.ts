import { expect, test } from "bun:test";
import { buildHistoryRecord, incoherence, readHistoryRecord } from "./types";
import { historyMetrics } from "./sort";
import { DEFAULT_CONFIG } from "../state/defaults";
import type { RunResult } from "../runner/contract";
import { testRunResult } from "../runner/test-helpers.testutil";

const throughput = {
  peakBytesPerSec: 120,
  stabilityPct: 3,
  totalBytes: 400,
  reportedBytesPerSec: 100,
  fullAverageBytesPerSec: 90,
  method: "full-average" as const,
  stabilityScore: 0.9,
  band: "high" as const,
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
  addedLatency: { download: 8, upload: -2, bidirectional: null },
  multiServer: {
    selection: [{ id: "a", name: "edge", url: "https://a.example" }],
    participants: ["a"],
    latencyFocus: "a",
    intervals: [],
    omittedIntervals: 0,
    failures: [
      {
        serverId: "a",
        stage: "upload",
        atMs: 5,
        scope: "throughput",
        reason: "timeout",
        message: "HTTP 503",
      },
      {
        serverId: "a",
        stage: "bidirectional",
        atMs: 9,
        scope: "throughput",
        reason: "insufficient-evidence",
        message: "Too little measured evidence for a result",
      },
    ],
    servers: [],
  },
  outcome: "incomplete",
  stages: {
    latency: "complete",
    download: "complete",
    upload: "failed",
    bidirectional: "partial",
  },
  startedAt: 100,
  durationMs: 80,
};

result.multiServer.servers = [
  {
    server: result.multiServer.selection[0],
    throughput: {
      origin: "https://a.example",
      transport: "fetch-stream",
      protocol: "http2",
    },
    latencyTarget: { origin: "https://a.example", transport: "websocket" },
    latency: result.latency,
    latencyByStage: result.latencyByStage,
    addedLatency: result.addedLatency,
    download: result.download,
    upload: result.upload,
    bidirectional: result.bidirectional,
    totalBytes: { down: 800, up: 0 },
    stages: result.stages,
  },
];

const saved = () =>
  buildHistoryRecord(result, { build: "b", engine: "e" }, undefined, 200);

test("a record keeps its run's result apart from the live one and reads back unchanged", () => {
  const source = structuredClone(result);
  const record = buildHistoryRecord(
    source,
    { build: "b", engine: "e" },
    undefined,
    200,
  );
  source.download!.reportedBytesPerSec = 1;
  expect(record.result.download?.reportedBytesPerSec).toBe(100);
  expect(readHistoryRecord(JSON.parse(JSON.stringify(record)))).toEqual(record);
  for (const schemaVersion of [undefined, 1, 2, 3, 6])
    expect(readHistoryRecord({ ...record, schemaVersion })).toBeNull();
});

test("a result cannot claim more than its stages and evidence support", () => {
  const config = {
    stages: {
      latency: true,
      download: true,
      upload: true,
      bidirectional: false,
    },
    duration: { ...DEFAULT_CONFIG.duration, downloadMs: 2_000 },
    adaptive: DEFAULT_CONFIG.adaptive,
  };
  expect(incoherence(result, config)).toEqual([
    "download is complete without 800 ms of evidence",
    "download covers 0 of 2000 ms",
    "bidirectional is partial but planned false",
  ]);
  const silent = structuredClone(result);
  silent.multiServer.failures = [];
  silent.outcome = "complete";
  expect(incoherence(silent)).toContain(
    "upload is failed without a stated reason",
  );
  expect(incoherence(silent)).toContain(
    "outcome complete should be incomplete",
  );
  expect(incoherence(testRunResult())).toContain("no stage ran");
});

test("records a view could not read or that are not plain data are skipped", () => {
  const valid = saved();
  const run = (multiServer: object) => ({
    ...valid,
    result: {
      ...valid.result,
      multiServer: { ...valid.result.multiServer, ...multiServer },
    },
  });
  const cases: unknown[] = [
    { ...valid, id: 5 },
    { ...valid, completedAt: 1e20 },
    { ...valid, engine: "x".repeat(4096) },
    { ...valid, result: null },
    { ...valid, result: { ...valid.result, stages: null } },
    { ...valid, result: { ...valid.result, durationMs: NaN } },
    { ...valid, result: { ...valid.result, latencyByStage: 5 } },
    { ...valid, result: { ...valid.result, bidirectional: { down: 5 } } },
    {
      ...valid,
      result: { ...valid.result, download: { reportedBytesPerSec: "fast" } },
    },
    { ...valid, result: { ...valid.result, outcome: "done" } },
    run({ selection: [{ id: "a", name: "edge", url: 5 }] }),
    run({ servers: [{}] }),
    run({ failures: [{ serverId: "a", stage: "warmup", reason: "timeout" }] }),
  ];
  for (const value of cases) expect(readHistoryRecord(value)).toBeNull();
});

test("a schema 4 record reads as a one-server result with its grade and wire model", () => {
  const lane = (center: number) => ({
    min: 9,
    max: 30,
    p10: 10,
    p90: 20,
    center,
    jitter: 2,
    timeoutRatio: 0,
    accountingComplete: true,
    timeoutCount: 0,
    unresolvedCount: 0,
    sendFailureCount: 0,
    count: 50,
  });
  const breakdown = {
    factors: [],
    transport: "http2",
    transportSource: "detected",
    framing: null,
    mtuBytes: 1500,
    ipVersion: 6,
    ipVersionSource: "detected",
  };
  const flat = {
    schemaVersion: 4,
    id: "old",
    startedAt: 100,
    completedAt: 200,
    durationMs: 100,
    stages: {
      latency: {
        status: "complete",
        result: {
          reportedMs: 12,
          jitterMs: 2,
          stabilityScore: 1,
          band: "high",
        },
        lanes: {
          latency: lane(12),
          download: lane(18),
          upload: null,
          bidirectional: null,
        },
      },
      download: {
        status: "complete",
        result: {
          reportedBytesPerSec: 1000,
          peakBytesPerSec: 1100,
          totalBytes: 5000,
          stabilityPct: 3,
        },
      },
      upload: { status: "failed", result: null },
      bidirectional: { status: "not-run", down: null, up: null },
    },
    bufferbloat: {
      idleMs: 12,
      loadedMs: 18,
      increaseMs: 6,
      grade: "A",
      addedMs: { download: 6 },
    },
    totalBytes: 5000,
    server: { name: "Home", location: "Here", engine: "e4" },
    transport: {
      throughput: { protocol: "h2", kind: "fetch-stream" },
      latency: { protocol: null, kind: "websocket" },
    },
    ipVersion: 6,
    client: { build: "v0.8.6" },
    failures: [{ stage: "upload", direction: "up", reason: "timeout" }],
    wireEstimates: {
      version: 2,
      breakdown: { download: breakdown, upload: null, bidirectional: null },
      downloadBytesPerSec: 1050,
      uploadBytesPerSec: null,
      bidirectionalBytesPerSec: null,
    },
  };
  const record = readHistoryRecord(flat)!;
  expect(record).toMatchObject({
    schemaVersion: 5,
    id: "old",
    completedAt: 200,
    build: "v0.8.6",
    engine: "e4",
  });
  expect(record.result).toMatchObject({
    outcome: "partial",
    stages: { latency: "complete", upload: "failed", bidirectional: "not-run" },
    bidirectional: null,
    latencyByStage: { download: { p50Ms: 18, probeCount: 50 }, upload: null },
    addedLatency: { download: 6, upload: null, bidirectional: null },
  });
  expect(JSON.stringify(record)).not.toContain("grade");
  expect(record.result.download?.wire?.totalMultiplier).toBeCloseTo(1.05);
  expect(record.result.multiServer.servers).toMatchObject([
    {
      server: { name: "Home", location: "Here" },
      throughput: { transport: "fetch-stream", clientIpVersion: 6 },
      latencyTarget: { transport: "websocket" },
      totalBytes: { down: 5000, up: 0 },
    },
  ]);
  expect(historyMetrics(record)).toMatchObject({
    download: 1000,
    idle: 12,
    loaded: 18,
  });
  const broken = { ...flat, stages: { ...flat.stages, upload: null } };
  expect(readHistoryRecord(broken)).toBeNull();
});
