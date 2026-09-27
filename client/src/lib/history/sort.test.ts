import { expect, test } from "bun:test";
import {
  naturalDescending,
  prepareHistorySort,
  sortPreparedHistory,
} from "./sort";
import { historyRecord } from "./test-helpers.testutil";
import { testRunResult } from "../runner/test-helpers.testutil";
import type {
  RunResult,
  StageLatencySummary,
  ThroughputResult,
} from "../runner/contract";

const lane = (reportedBytesPerSec: number): ThroughputResult => ({
  reportedBytesPerSec,
  peakBytesPerSec: null,
  totalBytes: 1,
  stabilityPct: 0,
});
const latency = (reportedMs: number) => ({
  reportedMs,
  jitterMs: 1,
  stabilityScore: 1,
  band: "high" as const,
});
const loaded = (lanes: Partial<Record<string, number>>) => ({
  ...testRunResult().latencyByStage,
  ...Object.fromEntries(
    Object.entries(lanes).map(([stage, p50Ms]) => [
      stage,
      { p50Ms } as StageLatencySummary,
    ]),
  ),
});
const record = (
  id: string,
  completedAt: number,
  result: Partial<RunResult> = {},
) => ({ ...historyRecord(), id, completedAt, result: testRunResult(result) });

test("all missing values sort last and ties are newest first", () => {
  const values = [
    record("old", 1, { download: lane(10) }),
    record("missing", 3),
    record("new", 2, { download: lane(10) }),
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
  const values = [
    record("a", 1, {
      download: lane(10),
      upload: lane(40),
      bidirectional: { down: lane(5), up: lane(5) },
      latency: latency(20),
      latencyByStage: loaded({ upload: 80 }),
    }),
    record("b", 2, {
      download: lane(30),
      upload: lane(20),
      bidirectional: { down: lane(20), up: lane(10) },
      latency: latency(10),
      // Loaded latency sorts by the highest loaded median, even without idle latency.
      latencyByStage: loaded({ download: 20, bidirectional: 5 }),
    }),
    // A large surviving lane must not outrank a complete bidirectional result.
    record("c", 3, {
      bidirectional: { down: lane(1_000), up: null },
      latency: latency(10),
    }),
  ];
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
