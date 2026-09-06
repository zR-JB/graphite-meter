import { expect, test } from "bun:test";
import { stubGlobals } from "../test-helpers.test";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
} from "../runner/test-helpers.test";
import { DEFAULT_CONFIG } from "../state/defaults";
import type { CoreHost } from "../runner/core";
import type { RunResult, RunnerEvent } from "../runner/contract";
import { isMultiServerResult } from "./serialization";
import { buildHistoryRecord, isHistoryRecord } from "../history/types";

async function run(
  options: {
    dropAt?: number;
    dropAll?: boolean;
    latencyFailure?: boolean;
    initialFailure?: boolean;
    laterPreparationFailure?: boolean;
  } = {},
) {
  const restore = stubGlobals(TEST_BUILD_TOKENS);
  const { ServerCoordinator } = await import("./coordinator");
  const calls: string[] = [];
  const events: RunnerEvent[] = [];
  const timers: ReturnType<typeof setTimeout>[] = [];
  const servers = ["a", "b"].map((id) => ({
    server: { id, url: `https://${id}.example`, name: id },
    paths: testPreparedPaths(),
  }));
  let index = 0;
  const coordinator = new ServerCoordinator(servers, "a", () => {
    const id = ["a", "b"][index++];
    let host: CoreHost;
    let last = 0;
    return {
      attach(value) {
        host = value;
      },
      onRunStart() {},
      onStageBegin(activity) {
        calls.push(`begin:${id}`);
        if (
          id === "a" &&
          (options.initialFailure ||
            (options.laterPreparationFailure && activity.stage === "upload"))
        )
          throw new Error("fixture preparation failure");
      },
      onStageMeasure() {
        last = performance.now();
        calls.push(`measure:${id}`);
        if (options.dropAt && (id === "a" || options.dropAll))
          timers.push(
            setTimeout(
              () =>
                host.failStage(
                  "download",
                  "connection-lost",
                  "fixture dropout",
                  "down",
                ),
              options.dropAt,
            ),
          );
        if (options.latencyFailure && id === "a")
          timers.push(
            setTimeout(() => host.ingestLatencyAccountingIncomplete(), 200),
          );
      },
      onStageEnd() {
        calls.push(`end:${id}`);
      },
      onAbort() {},
      onComplete() {},
      checkpoint: async () => {
        const at = Math.round(performance.now());
        return {
          id,
          bytes: at * 3,
          nanos: at * 1e6,
          requestedAtMs: at,
          receivedAtMs: at,
        };
      },
      flushDownload(now) {
        if (last) {
          host.ingestThroughput(
            "down",
            (now - last) * (id === "a" ? 1 : 3),
            (now - last) / 1000,
          );
          last = now;
        }
      },
    };
  });
  try {
    const result = await new Promise<RunResult>((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error("coordinator did not finish")),
        5000,
      );
      timers.push(timeout);
      coordinator.on((event) => {
        events.push(event);
        if (event.type === "complete") {
          clearTimeout(timeout);
          resolve(event.result);
        }
        if (event.type === "error") reject(event.error);
      });
      coordinator.start(
        {
          ...structuredClone(DEFAULT_CONFIG),
          stages: {
            latency: false,
            download: true,
            upload: !!options.laterPreparationFailure,
            bidirectional: false,
          },
          skipLoadedLatencyWhenStageOff: true,
          duration: {
            warmupMs: 0,
            latencyMs: 0,
            downloadMs: 1400,
            uploadMs: options.laterPreparationFailure ? 1400 : 0,
            bidirectionalMs: 0,
          },
          adaptive: { ...DEFAULT_CONFIG.adaptive, enabled: false },
        },
        0,
      );
    });
    return { result, calls, events };
  } finally {
    timers.forEach(clearTimeout);
    coordinator.dispose();
    restore();
  }
}
test("one coordinated stage reports the combined path, and v4 evidence survives serialization", async () => {
  const { result, calls, events } = await run();
  expect(calls.filter((value) => value.startsWith("begin:"))).toEqual([
    "begin:a",
    "begin:b",
  ]);
  expect(calls.filter((value) => value.startsWith("measure:"))).toEqual([
    "measure:a",
    "measure:b",
  ]);
  expect(
    events.filter(
      (event) => event.type === "phase" && event.transition.to === "download",
    ),
  ).toHaveLength(1);
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(4000, 0);
  expect(isMultiServerResult(result.multiServer)).toBe(true);
  const saved = buildHistoryRecord(result, {
    paths: null,
    clientBuild: "test",
  });
  expect(isHistoryRecord(JSON.parse(JSON.stringify(saved)))).toBe(true);
}, 6000);
test("a late dropout leaves the headline unavailable while retaining earlier measurements", async () => {
  const { result } = await run({ dropAt: 900 });
  expect(result.download).toBeNull();
  expect(result.multiServer?.participants).toEqual(["b"]);
  expect(result.multiServer?.intervals[0].full?.downBytesPerSec).toBeCloseTo(
    4000,
    0,
  );
  expect(result.multiServer?.failures[0].serverId).toBe("a");
  expect(
    isMultiServerResult(JSON.parse(JSON.stringify(result.multiServer))),
  ).toBe(true);
}, 6000);
test("all participants failing produces an incomplete result with both failures", async () => {
  const { result } = await run({ dropAt: 400, dropAll: true });
  expect(result.outcome).toBe("incomplete");
  expect(result.download).toBeNull();
  expect(result.multiServer?.participants).toEqual([]);
  expect(result.multiServer?.failures).toHaveLength(2);
}, 6000);
test("a latency-only failure preserves both throughput participants", async () => {
  const { result } = await run({ latencyFailure: true });
  expect(result.multiServer?.participants).toEqual(["a", "b"]);
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(4000, 0);
  expect(result.multiServer?.failures[0].scope).toBe("latency");
}, 6000);

test("initial preparation failure requires resolving the selection", async () => {
  await expect(run({ initialFailure: true })).rejects.toMatchObject({
    reason: "protocol-error",
  });
});

test("later preparation failure removes only its server and retains the completed stage", async () => {
  const { result } = await run({ laterPreparationFailure: true });
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(4000, 0);
  expect(result.upload?.reportedBytesPerSec).toBeCloseTo(3000, 0);
  expect(result.multiServer?.participants).toEqual(["b"]);
  expect(result.multiServer?.failures).toMatchObject([
    {
      serverId: "a",
      stage: "upload",
      scope: "throughput",
      reason: "preparation-failed",
    },
  ]);
}, 6000);

test("a primary latency server owns probes while every server still transfers", async () => {
  const restore = stubGlobals(TEST_BUILD_TOKENS);
  const { ServerCoordinator } = await import("./coordinator");
  const servers = ["a", "b"].map((id) => ({
    server: { id, name: id, url: `https://${id}.example` },
    paths: { ...testPreparedPaths(), ...(id === "a" ? { latency: null } : {}) },
  }));
  const calls: string[] = [];
  let index = 0;
  const coordinator = new ServerCoordinator(servers, "b", (paths) => {
    const id = servers[index++].server.id;
    let host: CoreHost;
    return {
      attach(value) {
        host = value;
      },
      onRunStart() {},
      onAbort() {},
      onComplete() {},
      onStageBegin(activity) {
        calls.push(`${id}:${activity.stage}`);
      },
      onStageMeasure() {
        if (paths.latency)
          for (let i = 0; i < 4; i++)
            host.ingestLatency({
              rttMs: 2,
              lost: false,
              observedAtMs: performance.now(),
            });
      },
      onStageEnd() {},
      checkpoint: async () => null,
      flushDownload() {
        host.ingestThroughput("down", 10000, 0.25);
      },
    };
  });
  try {
    const complete = new Promise<RunResult>((resolve, reject) =>
      coordinator.on((event) => {
        if (event.type === "complete") resolve(event.result);
        if (event.type === "error") reject(event.error);
      }),
    );
    coordinator.start(
      {
        ...structuredClone(DEFAULT_CONFIG),
        adaptive: { ...DEFAULT_CONFIG.adaptive, enabled: false },
        stages: {
          latency: true,
          download: true,
          upload: false,
          bidirectional: false,
        },
        duration: {
          ...DEFAULT_CONFIG.duration,
          warmupMs: 0,
          latencyMs: 50,
          downloadMs: 1000,
        },
      },
      2,
    );
    const result = await complete;
    expect(calls).toEqual(["b:latency", "a:download", "b:download"]);
    expect(result.multiServer?.servers[0].latencyTarget).toBeNull();
    expect(result.multiServer?.servers[0].latencyByStage.latency).toBeNull();
    expect(
      result.multiServer?.servers[1].latencyByStage.latency?.probeCount,
    ).toBe(4);
    expect(
      result.multiServer?.servers[1].latencyByStage.download?.probeCount,
    ).toBe(4);
    expect(result.multiServer?.participants).toEqual(["a", "b"]);
    expect(result.multiServer?.failures).toEqual([]);
  } finally {
    coordinator.dispose();
    restore();
  }
});
