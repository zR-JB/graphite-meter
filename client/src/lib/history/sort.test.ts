import { expect, test } from "bun:test";
import {
  naturalDescending,
  prepareHistorySort,
  sortPreparedHistory,
} from "./sort";
import type { HistoryRecord } from "./types";
import { historyRecord } from "./test-helpers.testutil";

function record(
  id: string,
  completedAt: number,
  down: number | null,
): HistoryRecord {
  const lane =
    down == null
      ? null
      : {
          reportedBytesPerSec: down,
          peakBytesPerSec: down,
          fullAverageBytesPerSec: down,
          method: "full-average" as const,
          totalBytes: 1,
          stabilityPct: 0,
          probeTimeoutPct: 0,
          stabilityScore: 1,
          band: "high" as const,
          serverAuthoritative: false,
        };
  const value = historyRecord();
  return {
    ...value,
    id,
    startedAt: completedAt - 1,
    completedAt,
    stages: {
      ...value.stages,
      download: { status: lane ? "complete" : "not-run", result: lane },
    },
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
  // A large surviving lane must not outrank a complete bidirectional result.
  values[2].stages.bidirectional = {
    status: "partial",
    down: { ...values[0].stages.download.result!, reportedBytesPerSec: 1_000 },
    up: null,
  };
  for (const value of values)
    value.stages.latency.result = {
      reportedMs: value.completedAt === 1 ? 20 : 10,
      minMs: 1,
      p50Ms: 2,
      p95Ms: 3,
      jitterMs: 1,
      probeTimeoutPct: 0,
      method: "full-average",
      stabilityScore: 1,
      band: "high",
    };
  const loaded = (center: number) => ({
    min: center,
    max: center,
    p10: center,
    p90: center,
    center,
    jitter: 0,
    timeoutRatio: 0,
    accountingComplete: true,
    timeoutCount: 0,
    unresolvedCount: 0,
    sendFailureCount: 0,
    count: 1,
  });
  // Loaded latency sorts by the highest loaded median, even without idle latency.
  values[0].stages.latency.lanes.upload = loaded(80);
  values[1].stages.latency.lanes.download = loaded(20);
  values[1].stages.latency.lanes.bidirectional = loaded(5);
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
      sortPreparedHistory(prepared, field, naturalDescending(field)).map(
        (item) => item.id,
      ),
      field,
    ).toEqual(expected);
  expect(sortPreparedHistory(prepared, "download", false).at(-1)?.id).toBe("c");
  expect(sortPreparedHistory(prepared, "loaded", false).at(-1)?.id).toBe("c");
});
