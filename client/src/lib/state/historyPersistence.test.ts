import "./runes.testutil";
import { expect, spyOn, test } from "bun:test";
import { DEFAULT_CONFIG } from "./defaults";
import {
  NOT_RUN,
  TEST_BUILD_TOKENS,
  testPreparedPaths,
  testRunResult,
} from "../runner/test-helpers.testutil";
import type { RunResult, ThroughputResult } from "../runner/contract";
import {
  LatencyPopulation,
  pathEvidence,
  ThroughputAggregate,
} from "../runner/measure";
import { singleLatencyBucket } from "../runner/series";

const throughput: ThroughputResult = {
  reportedBytesPerSec: 12_500_000,
  peakBytesPerSec: 13_000_000,
  totalBytes: 25_000_000,
  stabilityPct: 4,
};

function result(): RunResult {
  const aggregate = new ThroughputAggregate();
  aggregate.begin("download", ["self"], 0);
  aggregate.observe({ atMs: 0, down: { self: 0 }, up: {} });
  aggregate.observe({ atMs: 2000, down: { self: 25_000_000 }, up: {} });
  aggregate.result("download", false);
  const server = { id: "self", name: "Measured server", url: "http://a.test" };
  const empty = {
    latency: null,
    download: null,
    upload: null,
    bidirectional: null,
  };
  return testRunResult({
    download: { ...throughput },
    multiServer: {
      selection: [server],
      participants: [server.id],
      latencyFocus: server.id,
      intervals: aggregate.intervals,
      omittedIntervals: 0,
      failures: [
        {
          serverId: server.id,
          stage: "upload",
          atMs: 5,
          scope: "throughput",
          reason: "timeout",
          message: "HTTP 503",
        },
      ],
      servers: [
        {
          server,
          ...pathEvidence(testPreparedPaths()),
          latency: null,
          latencyByStage: empty,
          addedLatency: null,
          download: { ...throughput },
          upload: null,
          bidirectional: null,
          totalBytes: { down: 25_000_000, up: 0 },
          stages: { ...NOT_RUN, download: "complete" },
        },
      ],
    },
    outcome: "incomplete",
    stages: {
      latency: "not-run",
      download: "complete",
      upload: "failed",
      bidirectional: "not-run",
    },
    startedAt: 100,
    durationMs: 2_500,
  });
}

test("UI and history use the raw stage summary even when chart samples disagree", async () => {
  Object.assign(globalThis as Record<string, unknown>, TEST_BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const previousPreference = store.resultHistoryPreference;
  try {
    store.reset();
    store.resultHistoryPreference = "enabled";
    const raw = new LatencyPopulation();
    for (const rttMs of [10, 100, 10, 100])
      raw.observe({ rttMs, timedOut: false, observedAtMs: 0 });
    store.ingest({
      type: "serverLatency",
      serverId: store.latencyFocus,
      sample: {
        ...singleLatencyBucket(100, 55, false, "download"),
        underLoad: true,
      },
    });
    store.ingest({
      type: "serverLatencySummary",
      serverId: store.latencyFocus,
      stage: "download",
      summary: raw.summary(),
    });
    const lane = store.latencyLanes.find((lane) => lane.key === "download")!;
    expect(lane.min).toBe(10);
    expect(lane.p90).toBe(100);
    expect(lane.jitter).toBe(90);
    const completed = result();
    completed.latencyByStage.download = raw.summary();
    store.ingest({ type: "complete", result: completed });
    expect(store.stageResults.download).toEqual(completed.download);
    expect(store.stageResults.upload).toBeNull();
    expect(store.historyCandidate?.schemaVersion).toBe(5);
    expect(
      store.historyCandidate?.result.latencyByStage.download,
    ).toMatchObject({ minMs: 10, p90Ms: 100, jitterMs: 90, probeCount: 4 });
  } finally {
    store.resultHistoryPreference = previousPreference;
    store.reset();
  }
});

test("only an enabled complete event creates an immutable history candidate", async () => {
  Object.assign(globalThis as Record<string, unknown>, TEST_BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const previousPreference = store.resultHistoryPreference;
  try {
    store.reset();
    store.resultHistoryPreference = "enabled";
    const completed = result();
    const paths = testPreparedPaths();
    const config = {
      ...structuredClone(DEFAULT_CONFIG),
      stages: {
        latency: false,
        download: true,
        upload: true,
        bidirectional: false,
      },
    };
    const servers = [{ server: completed.multiServer.selection[0], paths }];
    store.run = { config, servers };
    store.ingest({ type: "complete", result: completed });
    const candidate = store.historyCandidate!;
    expect(candidate.engine).toBe(paths.discovery.engineVersion);
    expect(candidate.result.stages.upload).toBe("failed");
    expect(candidate.result.multiServer.failures[0]).toMatchObject({
      stage: "upload",
      reason: "timeout",
    });
    completed.download!.reportedBytesPerSec = 1;
    completed.multiServer.selection[0].name = "Next server";
    expect(candidate.result.multiServer.selection[0].name).toBe(
      "Measured server",
    );
    expect(JSON.stringify(candidate)).not.toContain(
      paths.throughput.probe.clientIp,
    );
    expect(candidate.result.download?.reportedBytesPerSec).toBe(12_500_000);

    store.reset();
    store.resultHistoryPreference = "enabled";
    const claimed = result();
    claimed.multiServer.failures = [];
    claimed.stages.upload = "not-run";
    claimed.outcome = "complete";
    store.run = { config, servers };
    const logged = spyOn(console, "error").mockImplementation(() => {});
    store.ingest({ type: "complete", result: claimed });
    logged.mockRestore();
    expect(store.historyCandidate!.result.outcome).toBe("incomplete");

    store.reset();
    store.resultHistoryPreference = "disabled";
    store.ingest({ type: "complete", result: result() });
    expect(store.historyCandidate).toBeNull();

    store.reset();
    store.resultHistoryPreference = "enabled";
    store.ingest({
      type: "error",
      error: {
        reason: "connection-lost",
        message: "failed run",
      },
    });
    expect(store.historyCandidate).toBeNull();
    store.ingest({
      type: "phase",
      transition: { to: "aborted", stage: null, t: 0 },
    });
    expect(store.historyCandidate).toBeNull();
  } finally {
    store.reset();
    store.resultHistoryPreference = previousPreference;
    for (const key of Object.keys(TEST_BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("settings reset returns history saving to the operator-controlled default", async () => {
  const { store } = await import("./store.svelte");
  const previousPreference = store.resultHistoryPreference;
  try {
    store.resultHistoryPreference = "enabled";
    store.restoreTestDisplayDefaults();
    expect(String(store.resultHistoryPreference)).toBe("default");

    store.resultHistoryPreference = "disabled";
    store.restoreTestDisplayDefaults();
    expect(String(store.resultHistoryPreference)).toBe("default");
  } finally {
    store.resultHistoryPreference = previousPreference;
  }
});
