import { expect, test } from "bun:test";
import { plugin, Transpiler } from "bun";
import { compileModule } from "svelte/compiler";
import type { ConnectionPresentation } from "../runner/connectionModel";
import type { RunResult, ThroughputResult } from "../runner/contract";

plugin({
  name: "history-store-runes",
  setup(build) {
    build.onLoad({ filter: /\.svelte\.ts$/ }, async (args) => {
      const source = await Bun.file(args.path).text();
      const module = new Transpiler({ loader: "ts" }).transformSync(source);
      return {
        contents: compileModule(module, {
          generate: "client",
          filename: args.path,
        }).js.code,
        loader: "js",
      };
    });
  },
});

const throughput: ThroughputResult = {
  meanBytesPerSec: 12_500_000,
  reportedBytesPerSec: 12_500_000,
  peakBytesPerSec: 13_000_000,
  fullAverageBytesPerSec: 12_000_000,
  method: "full-average",
  totalBytes: 25_000_000,
  stabilityPct: 4,
  packetLossPct: 0,
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
    bufferbloat: null,
    stageFailures: {
      upload: { stage: "upload", reason: "timeout", message: "private detail" },
    },
    startedAt: 100,
    durationMs: 2_500,
  };
}

function loopbackConnections(): Record<
  "throughput" | "latency",
  ConnectionPresentation
> {
  const connection = (
    role: "throughput" | "latency",
  ): ConnectionPresentation => ({
    role,
    selection: role === "throughput" ? "current" : "auto",
    target: null,
    availability: "not-advertised",
    validation: "verified",
    label: `${role} path`,
    summary: "Loopback test path",
    clientIp: "127.0.0.1",
    clientIpVersion: 4,
  });
  return {
    throughput: connection("throughput"),
    latency: connection("latency"),
  };
}

const BUILD_TOKENS = {
  __GM_ALLOW_DUMMY__: false,
  __GM_BUILD_PROFILE__: "test",
  __GM_RELEASE_VERSION__: null,
  __GM_SOURCE_REVISION__: "test-revision",
  __GM_BUILD_IDENTITY__: "test test-revision",
  __GM_CLIENT_VERSION__: "0.0.0-test",
} as const;

test("only an enabled complete event creates an immutable history candidate", async () => {
  Object.assign(globalThis as Record<string, unknown>, BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const previousPreference = store.resultHistoryPreference;
  try {
    store.reset();
    store.resultHistoryPreference = "enabled";
    const completed = result();
    store.ingest({ type: "complete", result: completed });
    const candidate = store.historyCandidate;
    expect(candidate?.stages.upload.status).toBe("failed");
    expect(candidate?.failures[0]).toEqual({
      stage: "upload",
      direction: null,
      reason: "timeout",
    });
    completed.download!.reportedBytesPerSec = 1;
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
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("wire snapshots are independent of their display preference", async () => {
  Object.assign(globalThis as Record<string, unknown>, BUILD_TOKENS);
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
    ).toBeGreaterThan(throughput.meanBytesPerSec);
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
    store.activeConnections = loopbackConnections();
    store.ingest({ type: "complete", result: result() });
    expect(store.historyCandidate?.wireEstimates).toBeNull();
  } finally {
    store.reset();
    store.resultHistoryPreference = previousPreference;
    store.showWireEstimates = previousShowWireEstimates;
    for (const key of Object.keys(BUILD_TOKENS))
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
