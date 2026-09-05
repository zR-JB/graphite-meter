import { expect, test } from "bun:test";
import type { RunResult, ThroughputResult } from "../runner/contract";
import {
  primaryResultGaugeArc,
  resultGaugeArcs,
  resultGaugeHeadPlacements,
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
  probeTimeoutPct: 0,
});

const result = (overrides: Partial<RunResult>): RunResult => ({
  download: null,
  upload: null,
  bidirectional: null,
  latency: null,
  latencyByStage: {
    latency: null,
    download: null,
    upload: null,
    bidirectional: null,
  },
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
          probeTimeoutPct: 0,
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

const headOptions = {
  baseRadius: 72,
  arcSweep: Math.PI * 1.5,
  headRadius: 3.3,
  borderWidth: 1,
};

const headDistance = (
  a: { fraction: number; radius: number },
  b: { fraction: number; radius: number },
): number => {
  const angleA = a.fraction * headOptions.arcSweep;
  const angleB = b.fraction * headOptions.arcSweep;
  return Math.hypot(
    a.radius * Math.cos(angleA) - b.radius * Math.cos(angleB),
    a.radius * Math.sin(angleA) - b.radius * Math.sin(angleB),
  );
};

test("result heads stay on the base radius when their endpoints are separated", () => {
  const placements = resultGaugeHeadPlacements([0, 0.5, 1], headOptions);
  expect(placements.map((placement) => placement.radius)).toEqual([72, 72, 72]);
  expect(placements.map((placement) => placement.fraction)).toEqual([
    0, 0.5, 1,
  ]);
});

test("the highest result always stays primary while equal and near-equal clusters use inward lanes", () => {
  for (const fractions of [
    [0.5, 0.5],
    [0.5, 0.5, 0.5],
    [0.5, 0.501, 0.502],
  ]) {
    const placements = resultGaugeHeadPlacements(fractions, headOptions);
    expect(placements[0]!.radius).toBe(headOptions.baseRadius);
    expect(
      placements.every(
        (placement, index) =>
          index === 0 || placement.radius <= headOptions.baseRadius,
      ),
    ).toBe(true);
    for (let i = 0; i < placements.length; i += 1) {
      for (let j = i + 1; j < placements.length; j += 1)
        expect(
          headDistance(placements[i]!, placements[j]!),
        ).toBeGreaterThanOrEqual(
          2 * (headOptions.headRadius + headOptions.borderWidth) + 2,
        );
    }
  }
});

test("result head placement is deterministic and bounded for compact gauge geometry", () => {
  const first = resultGaugeHeadPlacements([0.42, 0.42, 0.42], {
    ...headOptions,
    baseRadius: 36,
  });
  const second = resultGaugeHeadPlacements([0.42, 0.42, 0.42], {
    ...headOptions,
    baseRadius: 36,
  });
  expect(first).toEqual(second);
  expect(first[0]!.radius).toBe(36);
  expect(first.every((placement) => placement.radius >= 6.3)).toBe(true);
  expect(first.map((placement) => placement.fraction)).toEqual([
    0.42, 0.42, 0.42,
  ]);
});

test("headline prefers download then upload then bidirectional regardless of speed or paint order", () => {
  const download = arc("download", "Download", 10);
  const upload = arc("upload", "Upload", 20);
  const bidi = arc("bidirectional", "Bidirectional", 30);
  const arcs = sortResultGaugeArcs([download, upload, bidi]);
  expect(primaryResultGaugeArc(arcs)).toBe(download);
  expect(primaryResultGaugeArc([bidi, upload])).toBe(upload);
  expect(primaryResultGaugeArc([bidi])).toBe(bidi);
  expect(primaryResultGaugeArc([])).toBeNull();
  const partial = arc("bidirectional", "Bidirectional upload", 5, true);
  expect(primaryResultGaugeArc([partial])).toBe(partial);
});
