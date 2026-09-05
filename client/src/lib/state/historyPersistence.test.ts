import "./runes.test";
import { expect, test } from "bun:test";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
} from "../runner/test-helpers.test";
import type { RunResult, ThroughputResult } from "../runner/contract";
import { LatencyAccumulator } from "../runner/latencySummary";
import { singleLatencyBucket } from "../runner/latencyBuckets";

const throughput: ThroughputResult = {
  reportedBytesPerSec: 12_500_000,
  peakBytesPerSec: 13_000_000,
  fullAverageBytesPerSec: 12_000_000,
  method: "full-average",
  totalBytes: 25_000_000,
  stabilityPct: 4,
  probeTimeoutPct: 0,
  stabilityScore: 0.96,
  band: "high",
  serverAuthoritative: true,
};

function result(): RunResult {
  return {
    download: { ...throughput },
    upload: null,
    bidirectional: null,
    latency: null,
    latencyByStage: {
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    },
    bufferbloat: null,
    stageFailures: {
      upload: { stage: "upload", reason: "timeout", message: "private detail" },
    },
    startedAt: 100,
    durationMs: 2_500,
  };
}

test("UI and history use the raw stage summary even when chart samples disagree", async () => {
  Object.assign(globalThis as Record<string, unknown>, TEST_BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const previousPreference = store.resultHistoryPreference;
  try {
    store.reset();
    store.resultHistoryPreference = "enabled";
    const raw = new LatencyAccumulator();
    for (const rtt of [10, 100, 10, 100]) raw.observe(rtt, false, 0);
    store.ingest({
      type: "latency",
      sample: {
        ...singleLatencyBucket(100, 55, false, "download"),
        underLoad: true,
      },
    });
    store.ingest({
      type: "latencySummary",
      stage: "download",
      summary: raw.snapshot(),
    });
    const lane = store.latencyLanes.find((lane) => lane.key === "download")!;
    expect(lane.min).toBe(10);
    expect(lane.p90).toBe(100);
    expect(lane.jitter).toBe(90);
    const completed = result();
    completed.latencyByStage.download = raw.snapshot();
    store.ingest({ type: "complete", result: completed });
    expect(store.historyCandidate?.schemaVersion).toBe(3);
    expect(store.historyCandidate?.stages.latency.lanes.download).toMatchObject(
      { min: 10, p90: 100, jitter: 90, count: 4, timeoutRatio: 0 },
    );
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
    paths.discovery.server.name = "Measured server";
    store.activePaths = paths;
    store.ingest({ type: "complete", result: completed });
    const candidate = store.historyCandidate;
    expect(candidate?.stages.upload.status).toBe("failed");
    expect(candidate?.failures[0]).toEqual({
      stage: "upload",
      direction: null,
      reason: "timeout",
    });
    completed.download!.reportedBytesPerSec = 1;
    paths.discovery.server.name = "Next server";
    paths.throughput.probe.clientIp = "192.0.2.254";
    expect(candidate?.server.name).toBe("Measured server");
    expect(JSON.stringify(candidate)).not.toContain("192.0.2.254");
    expect(candidate?.stages.download.result?.reportedBytesPerSec).toBe(
      12_500_000,
    );

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
        phase: "download",
      },
    });
    expect(store.historyCandidate).toBeNull();
    store.ingest({
      type: "phase",
      transition: {
        from: "connecting",
        to: "aborted",
        stage: null,
        t: 0,
      },
    });
    expect(store.historyCandidate).toBeNull();
  } finally {
    store.reset();
    store.resultHistoryPreference = previousPreference;
    for (const key of Object.keys(TEST_BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("wire snapshots are independent of their display preference", async () => {
  Object.assign(globalThis as Record<string, unknown>, TEST_BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const previousPreference = store.resultHistoryPreference;
  const previousShowWireEstimates = store.showWireEstimates;
  try {
    store.reset();
    store.showWireEstimates = false;
    store.resultHistoryPreference = "enabled";
    store.ingest({ type: "complete", result: result() });
    const hiddenWireCandidate = store.historyCandidate;
    expect(
      hiddenWireCandidate?.wireEstimates?.downloadBytesPerSec,
    ).toBeGreaterThan(throughput.reportedBytesPerSec);
    expect(hiddenWireCandidate?.wireEstimates?.uploadBytesPerSec).toBeNull();
    expect(
      hiddenWireCandidate?.wireEstimates?.bidirectionalBytesPerSec,
    ).toBeNull();
    expect(JSON.stringify(hiddenWireCandidate)).not.toContain("127.0.0.1");

    store.showWireEstimates = true;
    expect(store.historyCandidate).toBe(hiddenWireCandidate);

    store.reset();
    store.showWireEstimates = false;
    store.resultHistoryPreference = "enabled";
    const loopback = testPreparedPaths();
    loopback.throughput.probe.clientIp = "127.0.0.1";
    loopback.latency!.probe.clientIp = "127.0.0.1";
    store.activePaths = loopback;
    store.ingest({ type: "complete", result: result() });
    expect(
      store.historyCandidate?.wireEstimates?.downloadBytesPerSec,
    ).toBeGreaterThan(throughput.reportedBytesPerSec);
    expect(store.historyCandidate?.wireEstimates?.uploadBytesPerSec).toBeNull();
    expect(JSON.stringify(store.historyCandidate)).not.toContain("127.0.0.1");
  } finally {
    store.reset();
    store.resultHistoryPreference = previousPreference;
    store.showWireEstimates = previousShowWireEstimates;
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
