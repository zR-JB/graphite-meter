import { expect, test } from "bun:test";
import type { RunResult, ThroughputResult } from "../runner/contract";
import { resultGaugeArcs } from "./resultGauge";

const throughput = (reportedBytesPerSec: number): ThroughputResult => ({
  meanBytesPerSec: reportedBytesPerSec,
  peakBytesPerSec: reportedBytesPerSec,
  stabilityPct: 100,
  totalBytes: reportedBytesPerSec,
  reportedBytesPerSec,
  fullAverageBytesPerSec: reportedBytesPerSec,
  method: "full-average",
  stabilityScore: 1,
  band: "high",
  packetLossPct: 0,
});

const result = (overrides: Partial<RunResult>): RunResult => ({
  download: null,
  upload: null,
  bidirectional: null,
  latency: null,
  bufferbloat: null,
  stageFailures: {},
  startedAt: 0,
  durationMs: 0,
  ...overrides,
});

test("terminal gauge enumerates every complete throughput phase", () => {
  expect(
    resultGaugeArcs(
      result({
        download: throughput(10),
        upload: throughput(20),
        bidirectional: { down: throughput(30), up: throughput(40) },
      }),
    ),
  ).toEqual([
    { phase: "download", label: "Download", bytesPerSec: 10, dashed: false },
    { phase: "upload", label: "Upload", bytesPerSec: 20, dashed: false },
    {
      phase: "bidirectional",
      label: "Bidirectional",
      bytesPerSec: 70,
      dashed: false,
    },
  ]);
});

test("one-sided bidirectional evidence stays partial", () => {
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: throughput(30), up: null } }),
    ),
  ).toEqual([
    {
      phase: "bidirectional",
      label: "Bidirectional download",
      bytesPerSec: 30,
      dashed: true,
    },
  ]);
});

test("terminal gauge skips unavailable stages in every combination", () => {
  expect(resultGaugeArcs(null)).toEqual([]);
  expect(
    resultGaugeArcs(
      result({
        latency: {
          idleMs: 10,
          minMs: 9,
          p50Ms: 10,
          p95Ms: 12,
          jitterMs: 1,
          packetLossPct: 0,
          reportedMs: 10,
          method: "full-average",
          stabilityScore: 1,
          band: "high",
        },
      }),
    ),
  ).toEqual([]);
  expect(resultGaugeArcs(result({ download: throughput(10) }))).toEqual([
    { phase: "download", label: "Download", bytesPerSec: 10, dashed: false },
  ]);
  expect(resultGaugeArcs(result({ upload: throughput(20) }))).toEqual([
    { phase: "upload", label: "Upload", bytesPerSec: 20, dashed: false },
  ]);
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: throughput(30), up: throughput(40) } }),
    ),
  ).toEqual([
    {
      phase: "bidirectional",
      label: "Bidirectional",
      bytesPerSec: 70,
      dashed: false,
    },
  ]);
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: null, up: throughput(40) } }),
    ),
  ).toEqual([
    {
      phase: "bidirectional",
      label: "Bidirectional upload",
      bytesPerSec: 40,
      dashed: true,
    },
  ]);
});
