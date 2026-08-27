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

const arc = (
  phase: "download" | "upload" | "bidirectional",
  label: string,
  bytesPerSec: number,
  dashed = false,
) => ({ phase, label, bytesPerSec, dashed });

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
    arc("bidirectional", "Bidirectional", 70),
    arc("upload", "Upload", 20),
    arc("download", "Download", 10),
  ]);
});

test("one-sided bidirectional evidence stays partial", () => {
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: throughput(30), up: null } }),
    ),
  ).toEqual([arc("bidirectional", "Bidirectional download", 30, true)]);
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
    arc("download", "Download", 10),
  ]);
  expect(resultGaugeArcs(result({ upload: throughput(20) }))).toEqual([
    arc("upload", "Upload", 20),
  ]);
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: throughput(30), up: throughput(40) } }),
    ),
  ).toEqual([arc("bidirectional", "Bidirectional", 70)]);
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: null, up: throughput(40) } }),
    ),
  ).toEqual([arc("bidirectional", "Bidirectional upload", 40, true)]);
});

test("layer ordering paints highest throughput first and preserves ties", () => {
  const layers = sortResultGaugeArcs([
    arc("download", "Download", 20),
    arc("upload", "Upload", 80),
    arc("bidirectional", "Bidirectional upload", 80, true),
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
    arc("bidirectional", "Bidirectional download", 30, true),
  ]);
});
