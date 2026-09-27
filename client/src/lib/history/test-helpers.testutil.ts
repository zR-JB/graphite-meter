import type { HistoryRecord } from "./types";
import { testRunResult } from "../runner/test-helpers.testutil";

export function historyRecord(index = 1, result = testRunResult()) {
  return {
    schemaVersion: 5,
    id: `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
    completedAt: index + 1,
    build: "b",
    engine: "e",
    result,
  } satisfies HistoryRecord;
}
