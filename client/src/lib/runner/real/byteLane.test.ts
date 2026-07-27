import { test, expect, mock } from "bun:test";
import type { LaneEvents } from "./byteLane";

// One fake worker per spawn, capturing what the session owner attaches to it.
const spawned: FakeWorker[] = [];

class FakeWorker {
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: ErrorEvent) => void) | null = null;
  posted: unknown[] = [];
  terminated = false;
  postMessage(msg: unknown): void {
    this.posted.push(msg);
  }
  terminate(): void {
    this.terminated = true;
  }
  /** Deliver a worker message the way the real worker would. */
  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

function spawn(): FakeWorker {
  const worker = new FakeWorker();
  spawned.push(worker);
  return worker;
}

// byteLane.ts imports the fetch-lane factories and stopWorker too, so the mock
// has to cover them: a partial factory fails to link when this file runs alone.
mock.module("./workerPool", () => ({
  wtTransferWorker: spawn,
  downloadWorker: spawn,
  uploadWorker: spawn,
  stopWorker: (worker: FakeWorker) => worker.terminate(),
}));

const { sessionLane } = await import("./byteLane");

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

// A dying session reaches every lane reader, the accept loop and the close
// promise. Reporting each would cost the caller a retry per reader and can
// exhaust the early-fail budget on a link that is merely flapping.
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

// discard() terminates the worker, but messages already queued would still be
// dispatched to a handler that no longer speaks for this lane.
test("a discarded worker stops reaching its owner", () => {
  const { lane, errors, worker } = session();
  lane.discard();
  worker.emit({ type: "error", recoverable: true, detail: "late error" });

  expect(worker.terminated).toBe(true);
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
