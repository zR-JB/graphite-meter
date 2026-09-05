import type { HistoryRecord } from "./types";

export function historyRecord(index = 1): HistoryRecord {
  return {
    schemaVersion: 1,
    id: `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
    startedAt: index,
    completedAt: index + 1,
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
