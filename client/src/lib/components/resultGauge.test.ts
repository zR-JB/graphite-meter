import { expect, test } from "bun:test";
import type { RunResult, ThroughputResult } from "../runner/contract";
import {
  resultGaugeArcs,
  resultGaugeFillTarget,
  sortResultGaugeArcs,
} from "./resultGauge";

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
    {
      phase: "bidirectional",
      label: "Bidirectional",
      bytesPerSec: 70,
      dashed: false,
    },
    { phase: "upload", label: "Upload", bytesPerSec: 20, dashed: false },
    { phase: "download", label: "Download", bytesPerSec: 10, dashed: false },
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

test("layer ordering paints highest throughput first and preserves ties", () => {
  const layers = sortResultGaugeArcs([
    { phase: "download", label: "Download", bytesPerSec: 20, dashed: false },
    { phase: "upload", label: "Upload", bytesPerSec: 80, dashed: false },
    {
      phase: "bidirectional",
      label: "Bidirectional upload",
      bytesPerSec: 80,
      dashed: true,
    },
  ]);
  expect(layers.map((arc) => arc.phase)).toEqual([
    "upload",
    "bidirectional",
    "download",
  ]);
  expect(layers[1]!.dashed).toBe(true);
});

test("one, two, and three results all produce bounded layered fill", () => {
  expect(resultGaugeFillTarget([])).toBe(0);
  expect(resultGaugeFillTarget([0.4])).toBe(0.4);
  expect(resultGaugeFillTarget([0.2, 0.8])).toBe(0.8);
  expect(resultGaugeFillTarget([0.1, 0.7, 1.4])).toBe(1);
  expect(resultGaugeFillTarget([-1, Number.NaN])).toBe(0);
});

test("partial result styling remains dashed without marker geometry", () => {
  const arcs = resultGaugeArcs(
    result({ bidirectional: { down: throughput(30), up: null } }),
  );
  expect(arcs).toEqual([
    {
      phase: "bidirectional",
      label: "Bidirectional download",
      bytesPerSec: 30,
      dashed: true,
    },
  ]);
});
