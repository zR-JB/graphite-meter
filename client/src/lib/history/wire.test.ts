import { expect, test } from "bun:test";
import {
  combineCompensationEstimates,
  estimateCompensation,
} from "../compensation";
import type { RunResult } from "../runner/contract";
import { buildHistoryRecord, isHistoryRecord } from "./types";
import {
  historyWireEstimates,
  historyWirePresentation,
  isWireEstimates,
} from "./wire";

function result(): RunResult {
  return {
    download: {
      reportedBytesPerSec: 1_000_000,
      fullAverageBytesPerSec: 1_000_000,
      peakBytesPerSec: 1_000_000,
      totalBytes: 1_000_000,
      method: "full-average",
      stabilityPct: 100,
      stabilityScore: 1,
      band: "high",
      probeTimeoutPct: null,
      serverAuthoritative: true,
    },
    upload: null,
    latency: null,
    bidirectional: null,
    latencyByStage: {
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    },
    bufferbloat: null,
    stageFailures: {},
    startedAt: 100,
    durationMs: 1000,
  };
}

test("saved wire models survive reload without using a later connection or mutable estimate", () => {
  const estimate = estimateCompensation(1_000_000, "h2", true, 6);
  const wireEstimates = historyWireEstimates(estimate, null, null);
  const record = buildHistoryRecord(result(), {
    paths: null,
    clientBuild: "test",
    wireEstimates,
  });
  const reloaded = JSON.parse(JSON.stringify(record));
  expect(isHistoryRecord(reloaded)).toBe(true);
  const before = historyWirePresentation(reloaded, "download")!;
  expect(before.pct).toBe(
    `+${((estimate.totalMultiplier - 1) * 100).toFixed(1)}%`,
  );
  expect(before.tooltip).toContain("TLS 1.3 records +");
  expect(before.tooltip).toContain("IPv6 +");
  estimate.factors[0].contributionPct = 90;
  expect(historyWirePresentation(record, "download")).toEqual(before);
  expect(historyWirePresentation(record, "upload")).toBeNull();
});

test("old snapshots show their saved percentage and identify missing breakdowns", () => {
  const record = buildHistoryRecord(result(), {
    paths: null,
    clientBuild: "old",
    wireEstimates: {
      version: 1,
      downloadBytesPerSec: 1_063_000,
      uploadBytesPerSec: null,
      bidirectionalBytesPerSec: null,
    },
  });
  expect(historyWirePresentation(record, "download")).toMatchObject({
    pct: "+6.3%",
    bytesPerSec: 1_063_000,
  });
  expect(historyWirePresentation(record, "download")?.tooltip).toContain(
    "Per-part breakdown unavailable",
  );
  record.stages.download.result!.reportedBytesPerSec = 0;
  expect(historyWirePresentation(record, "download")?.pct).toBeNull();
});

test("combined history percentages use the sum of both lanes and retain weighted components", () => {
  const run = result();
  const down = estimateCompensation(3_000_000, "http/1.1", false, 4);
  const up = estimateCompensation(1_000_000, "h3", true, 6);
  run.bidirectional = {
    down: { ...run.download!, reportedBytesPerSec: down.measuredBytesPerSec },
    up: { ...run.download!, reportedBytesPerSec: up.measuredBytesPerSec },
  };
  const combined = combineCompensationEstimates([down, up]);
  const record = buildHistoryRecord(run, {
    paths: null,
    clientBuild: "test",
    wireEstimates: historyWireEstimates(null, null, combined),
  });
  expect(historyWirePresentation(record, "bidirectional")?.pct).toBe(
    `+${((combined.totalMultiplier - 1) * 100).toFixed(1)}%`,
  );
  expect(historyWirePresentation(record, "bidirectional")?.tooltip).toContain(
    "Transport headers +",
  );
  record.stages.bidirectional.up = null;
  expect(historyWirePresentation(record, "bidirectional")).toBeNull();
});

test("wire model import validates bounded factors, provenance, and the version boundary", () => {
  const valid = historyWireEstimates(
    estimateCompensation(1_000_000, "h2", true, 4),
    null,
    null,
  )!;
  expect(isWireEstimates(valid)).toBe(true);
  const invalid: unknown[] = [
    { ...valid, version: 1 },
    { ...valid, version: 3 },
    { ...valid, downloadBytesPerSec: Infinity },
    { ...valid, breakdown: {} },
  ];
  if (valid.version !== 2) throw new Error("expected current snapshot");
  for (const patch of [
    { transport: ["http2"] },
    { ipVersion: "4" },
    { framing: {} },
    { componentCount: 65 },
    { factors: Array(6).fill(valid.breakdown.download!.factors[0]) },
    {
      factors: [
        { ...valid.breakdown.download!.factors[0], contributionPct: NaN },
      ],
    },
    {
      factors: [
        { ...valid.breakdown.download!.factors[0], label: "x".repeat(129) },
      ],
    },
    { unexpected: "data" },
  ])
    invalid.push({
      ...valid,
      breakdown: {
        ...valid.breakdown,
        download: { ...valid.breakdown.download, ...patch },
      },
    });
  for (const value of invalid) expect(isWireEstimates(value)).toBe(false);
});
