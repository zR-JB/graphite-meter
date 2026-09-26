import { expect, test } from "bun:test";
import {
  combineCompensationEstimates,
  estimateCompensation,
} from "../compensation";
import type { RunResult } from "../runner/contract";
import { testRunResult } from "../runner/test-helpers.testutil";
import { buildHistoryRecord, isHistoryRecord } from "./types";
import { historyWireEstimates, historyWirePresentation } from "./wire";

function result(): RunResult {
  return testRunResult({
    download: {
      reportedBytesPerSec: 1_000_000,
      fullAverageBytesPerSec: 1_000_000,
      peakBytesPerSec: 1_000_000,
      totalBytes: 1_000_000,
      method: "full-average",
      stabilityPct: 100,
      stabilityScore: 1,
      band: "high",
      serverAuthoritative: true,
    },
    startedAt: 100,
    durationMs: 1000,
  });
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

test("current snapshots show their saved percentage and identify nullable breakdowns", () => {
  const record = buildHistoryRecord(result(), {
    paths: null,
    clientBuild: "test",
    wireEstimates: {
      version: 2,
      breakdown: { download: null, upload: null, bidirectional: null },
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
  // Like the live card, an overhead under 0.5% is not shown.
  record.stages.download.result!.reportedBytesPerSec = 1_060_000;
  expect(historyWirePresentation(record, "download")).toBeNull();
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
