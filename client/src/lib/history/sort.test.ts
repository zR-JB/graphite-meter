import { expect, test } from "bun:test";
import {
  naturalDescending,
  prepareHistorySort,
  sortPreparedHistory,
} from "./sort";
import type { HistoryRecordV1 } from "./types";

function record(
  id: string,
  completedAt: number,
  down: number | null,
): HistoryRecordV1 {
  const lane =
    down == null
      ? null
      : {
          meanBytesPerSec: down,
          reportedBytesPerSec: down,
          peakBytesPerSec: down,
          fullAverageBytesPerSec: down,
          method: "full-average" as const,
          totalBytes: 1,
          stabilityPct: 0,
          packetLossPct: 0,
          stabilityScore: 1,
          band: "high" as const,
          serverAuthoritative: false,
        };
  const latency = {
    status: "not-run" as const,
    result: null,
    lanes: { latency: null, download: null, upload: null, bidirectional: null },
  };
  return {
    schemaVersion: 1,
    id,
    startedAt: completedAt - 1,
    completedAt,
    durationMs: 1,
    stages: {
      latency,
      download: { status: lane ? "complete" : "not-run", result: lane },
      upload: { status: "not-run", result: null },
      bidirectional: { status: "not-run", down: null, up: null },
    },
    bufferbloat: null,
    totalBytes: 1,
    server: { name: "s", location: null, engine: "e" },
    transport: {
      throughput: { protocol: null, kind: null },
      latency: { protocol: null, kind: null },
    },
    ipVersion: null,
    client: { build: "b" },
    failures: [],
    wireEstimates: null,
  };
}
test("all missing values sort last and ties are newest first", () => {
  const values = [
    record("old", 1, 10),
    record("missing", 3, null),
    record("new", 2, 10),
  ];
  const prepared = prepareHistorySort(values);
  expect(
    sortPreparedHistory(prepared, "download", true).map((value) => value.id),
  ).toEqual(["new", "old", "missing"]);
  expect(
    sortPreparedHistory(prepared, "download", false).map((value) => value.id),
  ).toEqual(["new", "old", "missing"]);
});

test("natural directions prefer newest and faster throughput but lower latency", () => {
  expect(naturalDescending("date")).toBe(true);
  expect(naturalDescending("download")).toBe(true);
  expect(naturalDescending("upload")).toBe(true);
  expect(naturalDescending("bidirectional")).toBe(true);
  expect(naturalDescending("idle")).toBe(false);
  expect(naturalDescending("loaded")).toBe(false);
});

test("each history field sorts in its natural direction and keeps nulls last", () => {
  const values = [record("a", 1, 10), record("b", 2, 30), record("c", 3, null)];
  values[0].stages.upload = {
    status: "complete",
    result: { ...values[0].stages.download.result!, reportedBytesPerSec: 40 },
  };
  values[1].stages.upload = {
    status: "complete",
    result: { ...values[1].stages.download.result!, reportedBytesPerSec: 20 },
  };
  values[0].stages.bidirectional = {
    status: "complete",
    down: { ...values[0].stages.download.result!, reportedBytesPerSec: 5 },
    up: { ...values[0].stages.download.result!, reportedBytesPerSec: 5 },
  };
  values[1].stages.bidirectional = {
    status: "complete",
    down: { ...values[1].stages.download.result!, reportedBytesPerSec: 20 },
    up: { ...values[1].stages.download.result!, reportedBytesPerSec: 10 },
  };
  for (const value of values)
    value.stages.latency.result = {
      reportedMs: value.completedAt === 1 ? 20 : 10,
      minMs: 1,
      p50Ms: 2,
      p95Ms: 3,
      jitterMs: 1,
      packetLossPct: 0,
      method: "full-average",
      stabilityScore: 1,
      band: "high",
    };
  values[0].bufferbloat = {
    idleMs: 1,
    loadedMs: 80,
    increaseMs: 79,
    grade: "B",
  };
  values[1].bufferbloat = {
    idleMs: 1,
    loadedMs: 20,
    increaseMs: 19,
    grade: "A",
  };
  const prepared = prepareHistorySort(values);
  const natural: [Parameters<typeof sortPreparedHistory>[1], string[]][] = [
    ["date", ["c", "b", "a"]],
    ["download", ["b", "a", "c"]],
    ["upload", ["a", "b", "c"]],
    ["bidirectional", ["b", "a", "c"]],
    ["idle", ["c", "b", "a"]],
    ["loaded", ["b", "a", "c"]],
  ];
  for (const [field, expected] of natural)
    expect(
      sortPreparedHistory(
        prepared,
        field,
        field === "idle" || field === "loaded" ? false : true,
      ).map((item) => item.id),
      field,
    ).toEqual(expected);
  expect(sortPreparedHistory(prepared, "download", false).at(-1)?.id).toBe("c");
  expect(sortPreparedHistory(prepared, "loaded", false).at(-1)?.id).toBe("c");
});

test("prepared sorting extracts each record's active metric once across repeated orders", () => {
  let downloadReads = 0;
  const values = Array.from({ length: 2_000 }, (_, index) => {
    const item = record(String(index), index, index);
    Object.defineProperty(item.stages.download.result!, "reportedBytesPerSec", {
      enumerable: true,
      get: () => {
        downloadReads++;
        return index;
      },
    });
    return item;
  });

  const prepared = prepareHistorySort(values);
  expect(downloadReads).toBe(values.length);
  expect(sortPreparedHistory(prepared, "download", true).at(0)?.id).toBe(
    "1999",
  );
  expect(sortPreparedHistory(prepared, "download", false).at(0)?.id).toBe("0");
  expect(sortPreparedHistory(prepared, "date", true).at(0)?.id).toBe("1999");
  expect(downloadReads).toBe(values.length);
});
