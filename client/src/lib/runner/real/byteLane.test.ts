import { test, expect, mock } from "bun:test";
import type { LaneEvents } from "./byteLane";
import { TestWorker } from "./test-helpers.test";

// One fake worker per spawn, capturing what the session owner attaches to it.
const spawned: TestWorker[] = [];

function spawn(): TestWorker {
  const worker = new TestWorker();
  spawned.push(worker);
  return worker;
}

// byteLane.ts imports every worker factory, so the mock has to cover them.
mock.module("./workerPool", () => ({
  wtTransferWorker: spawn,
  downloadWorker: spawn,
  uploadWorker: spawn,
  pingWorker: () => new Worker("", { type: "module" }),
}));

const { fetchLane, sessionLane } = await import("./byteLane");

const OPTS = {
  url: "https://meter/wt/download",
  dir: "down" as const,
  lanes: 1,
  datagrams: false,
};

function session(onProgress: LaneEvents["onProgress"] = () => {}) {
  spawned.length = 0;
  const errors: string[] = [];
  const lane = sessionLane(OPTS, {
    onProgress,
    onAlive: () => {},
    onError: (_recoverable, detail) => errors.push(detail),
    onUploadProgress: () => {},
    onAuthRequired: () => {},
  });
  lane.start();
  return { lane, errors, worker: spawned[0] };
}

// A dying session reaches every lane reader, the accept loop and the close promise.
test("one session death reports one error however many readers see it", () => {
  const { errors, worker } = session();
  worker.emit({ type: "established" });
  for (const detail of [
    "lane 1 read failed",
    "accept loop failed",
    "session closed",
  ])
    worker.emit({ type: "error", recoverable: true, detail });

  expect(errors).toEqual(["lane 1 read failed"]);
});

test("a restart reports the next generation's first failure", () => {
  const { lane, errors, worker } = session();
  worker.emit({ type: "error", recoverable: true, detail: "first death" });
  lane.start();
  spawned[1].emit({ type: "error", recoverable: true, detail: "second death" });

  expect(errors).toEqual(["first death", "second death"]);
});

// Queued worker events must not reach a discarded lane's owner.
test("a discarded worker stops reaching its owner", () => {
  const { lane, errors, worker } = session();
  lane.discard();
  worker.emit({ type: "error", recoverable: true, detail: "late error" });

  expect(worker.terminated).toBeGreaterThan(0);
  expect(errors).toEqual([]);
});

test("graceful stop relays the worker's final download progress", async () => {
  const progress: [number, number | undefined, number | undefined][] = [];
  const { lane, worker } = session((bytes, elapsedMs, seq) =>
    progress.push([bytes, elapsedMs, seq]),
  );

  const stopping = lane.stop();
  worker.emit({ type: "progress", bytes: 17, elapsedMs: 25, seq: 3 });
  worker.emit({ type: "stopped" });
  await stopping;

  expect(progress).toEqual([[17, 25, 3]]);
});

test("an upload worker's local completion metadata stays on the alive seam", () => {
  spawned.length = 0;
  const hints: [number | undefined, number | undefined][] = [];
  const lane = fetchLane(
    {
      url: "https://meter/upload",
      dir: "up",
      lanes: 1,
      credentials: "same-origin",
    },
    {
      onProgress: () => {
        throw new Error("local upload completion must not be byte progress");
      },
      onAlive: (bytes, elapsedMs) => hints.push([bytes, elapsedMs]),
      onError: () => {},
      onUploadProgress: () => {},
      onAuthRequired: () => {},
    },
  );
  lane.start();
  spawned[0].emit({ type: "alive", bytes: 512, elapsedMs: 40 });

  expect(hints).toEqual([[512, 40]]);
});

test("a restarted fetch lane detaches the prior worker immediately", async () => {
  spawned.length = 0;
  const progress: number[] = [];
  const errors: string[] = [];
  const lane = fetchLane(
    {
      url: "https://meter/download",
      dir: "down",
      lanes: 1,
      credentials: "same-origin",
    },
    {
      onProgress: (bytes) => progress.push(bytes),
      onAlive: () => {},
      onError: (_recoverable, detail) => errors.push(detail),
      onUploadProgress: () => {},
      onAuthRequired: () => {},
    },
  );
  lane.start();
  const old = spawned[0];
  lane.start();
  old.emit({ type: "progress", bytes: 100 });
  old.onerror?.({ message: "late failure" } as ErrorEvent);
  expect(old.terminated).toBe(1);
  expect(progress).toEqual([]);
  expect(errors).toEqual([]);
  const current = spawned[1];
  current.emit({ type: "progress", bytes: 20 });
  const stopped = lane.stop();
  current.emit({ type: "progress", bytes: 200 });
  expect(progress).toEqual([20]);
  expect(current.onmessage).toBeNull();
  await stopped;
});
