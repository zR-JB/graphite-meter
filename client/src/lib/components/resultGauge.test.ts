import { expect, test } from "bun:test";
import type { ThroughputResult } from "../runner/contract";
import { testRunResult } from "../runner/test-helpers.testutil";
import {
  primaryResultGaugeArc,
  resultGaugeArcs,
  resultGaugeHeads,
  sortResultGaugeArcs,
} from "./resultGauge";

const throughput = (reportedBytesPerSec: number): ThroughputResult => ({
  peakBytesPerSec: reportedBytesPerSec,
  stabilityPct: 100,
  totalBytes: reportedBytesPerSec,
  reportedBytesPerSec,
});

const result = testRunResult;

const arc = (
  phase: "download" | "upload" | "bidirectional",
  label: string,
  bytesPerSec: number,
  direction: "download" | "upload" | "bidirectional" = phase,
) => ({ phase, direction, label, bytesPerSec, dashed: direction !== phase });

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

test("one-sided bidirectional evidence stays partial and a zero lane is still evidence", () => {
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: throughput(30), up: null } }),
    ),
  ).toEqual([arc("bidirectional", "Bidirectional download", 30, "download")]);
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: null, up: throughput(0) } }),
    ),
  ).toEqual([arc("bidirectional", "Bidirectional upload", 0, "upload")]);
  expect(
    resultGaugeArcs(
      result({ bidirectional: { down: throughput(0), up: throughput(40) } }),
    ),
  ).toEqual([arc("bidirectional", "Bidirectional", 40)]);
});

test("terminal gauge skips unavailable stages in every combination", () => {
  expect(resultGaugeArcs(null)).toEqual([]);
  expect(
    resultGaugeArcs(
      result({
        latency: {
          jitterMs: 1,
          reportedMs: 10,
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
});

// A 72 px ring with 8 px heads: a fraction of 0.01 lies 3.4 px along it.
const ring = { radius: 72, arcSweep: Math.PI * 1.5, headRadius: 8 };

test("result heads stay whole beads on the ring when apart, and lie over a close higher one", () => {
  expect(resultGaugeHeads([1, 0.5, 0], ring)).toEqual(["bead", "bead", "bead"]);
  // Highest first: each lower head is judged against every head painted before it.
  expect(resultGaugeHeads([0.6, 0.5, 0.46], ring)).toEqual([
    "bead",
    "bead",
    "stacked",
  ]);
  expect(resultGaugeHeads([0.5, 0.5, 0.49], ring)).toEqual([
    "bead",
    "split",
    "split",
  ]);
  expect(resultGaugeHeads([Number.NaN, 0], ring)).toEqual(["bead", "split"]);
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
  const partial = arc("bidirectional", "Bidirectional upload", 5, "upload");
  expect(primaryResultGaugeArc([partial])).toBe(partial);
});
