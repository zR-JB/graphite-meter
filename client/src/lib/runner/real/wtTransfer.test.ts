import { test, expect, mock } from "bun:test";

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

mock.module("./workerPool", () => ({
  wtTransferWorker: () => {
    const worker = new FakeWorker();
    spawned.push(worker);
    return worker;
  },
}));

const { WtTransferSession } = await import("./wtTransfer");

function session() {
  spawned.length = 0;
  const errors: string[] = [];
  const owner = new WtTransferSession({
    onProgress: () => {},
    onAlive: () => {},
    onError: (_recoverable, detail) => errors.push(detail),
    onUploadProgress: () => {},
  });
  owner.start({
    url: "https://meter/wt/download",
    dir: "down",
    lanes: 1,
    datagrams: false,
  });
  return { owner, errors, worker: spawned[0] };
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
  const { owner, errors, worker } = session();
  worker.emit({ type: "error", recoverable: true, detail: "first death" });
  owner.start({
    url: "https://meter/wt/download",
    dir: "down",
    lanes: 1,
    datagrams: false,
  });
  spawned[1].emit({ type: "error", recoverable: true, detail: "second death" });

  expect(errors).toEqual(["first death", "second death"]);
});

// discard() terminates the worker, but messages already queued would still be
// dispatched to a handler that no longer speaks for this lane.
test("a discarded worker stops reaching its owner", () => {
  const { owner, errors, worker } = session();
  owner.discard();
  worker.emit({ type: "error", recoverable: true, detail: "late error" });

  expect(worker.terminated).toBe(true);
  expect(errors).toEqual([]);
});
