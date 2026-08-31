import { expect, test } from "bun:test";
import { HISTORY_LIMIT } from "./types";
import { retainNewest } from "./repository";
import type { HistoryRecordV1 } from "./types";

function record(index: number): HistoryRecordV1 {
  return {
    schemaVersion: 1,
    id: `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
    startedAt: index,
    completedAt: index,
    durationMs: 1,
    stages: {
      latency: {
        status: "not-run",
        result: null,
        lanes: {
          latency: null,
          download: null,
          upload: null,
          bidirectional: null,
        },
      },
      download: { status: "not-run", result: null },
      upload: { status: "not-run", result: null },
      bidirectional: { status: "not-run", down: null, up: null },
    },
    bufferbloat: null,
    totalBytes: 0,
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

test("retention keeps exactly the newest 2,000 records", () => {
  const kept = retainNewest(
    Array.from({ length: HISTORY_LIMIT + 17 }, (_, index) => record(index)),
  );
  expect(kept).toHaveLength(HISTORY_LIMIT);
  expect(kept[0].completedAt).toBe(HISTORY_LIMIT + 16);
  expect(kept.at(-1)?.completedAt).toBe(17);
});
