// Teardown corners of the upload meter that the RealRunner and wtStage
// integration pins do not reach: a superseded external attach, and a discard
// arriving while a finalizing teardown still holds the worker.
import { test, expect } from "bun:test";
import type { CoreHost } from "../core";
import type { FetchThroughputTarget } from "../../api/endpoints";
import {
  UploadProgressChannel,
  type UploadProgressLane,
} from "./uploadProgress";

class FakeWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  readonly sent: unknown[] = [];
  terminated = 0;
  static last: FakeWorker | null = null;

  constructor() {
    FakeWorker.last = this;
  }
  postMessage(message: unknown): void {
    this.sent.push(message);
  }
  terminate(): void {
    this.terminated++;
  }
}

const target: FetchThroughputTarget = {
  id: "http://meter.test:7246",
  origin: "http://meter.test:7246",
  transport: "fetch-stream",
  protocol: "http1",
  tls: false,
  routes: {
    probe: "/probe",
    download: "/download",
    upload: "/upload",
    uploadSession: "/upload/session",
    uploadProgress: "/upload/progress",
  },
};

function channelUnderTest(): {
  channel: UploadProgressChannel;
  failures: string[];
} {
  const failures: string[] = [];
  const lane: UploadProgressLane = {
    stage: "upload",
    measuring: false,
    stageSawBytes: false,
  };
  const host = {
    failStage(_stage: string, _reason: string, message: string) {
      failures.push(message);
    },
    fail(_reason: string, message: string) {
      failures.push(message);
    },
    ingestThroughput() {},
  } as unknown as CoreHost;
  return {
    channel: new UploadProgressChannel({
      host: () => host,
      target: () => target,
      lane: () => lane,
      transferActive: () => true,
      discardTransfer: () => {},
      setLaneStalled: () => {},
    }),
    failures,
  };
}

// Only one owner may act on an attach outcome, so a replaced or torn-down feed
// resolves "superseded" rather than failing the stage on its establish timeout.
test("attachExternal: a replaced feed is superseded, not a stage failure", async () => {
  const { channel, failures } = channelUnderTest();
  const first = channel.attachExternal(() => {});
  const second = channel.attachExternal(() => {});
  expect(await first).toBe("superseded");

  await channel.teardown(false);
  expect(await second).toBe("superseded");
  expect(failures).toEqual([]);
});

// Unreachable today (every path tears down first), but a worker left running
// under a new feed would keep pushing its own upload id's cumulative count into
// the next stage's meter, which the monotonic guard accepts.
test("taking a session feed terminates the worker feed it replaces", async () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
    const { channel, failures } = channelUnderTest();
    const primed = channel.prime("upload", "gmu_one");
    const worker = FakeWorker.last!;

    const attached = channel.attachExternal(() => {});
    expect(worker.terminated).toBe(1);

    await channel.teardown(false);
    expect(await attached).toBe("superseded");
    expect(await primed).toBe(false);
    expect(failures).toEqual([]);
  } finally {
    globalThis.Worker = realWorker;
  }
});

// A discarded stage arriving mid-finalize must resolve the pending grace rather
// than start a second one, and terminate the worker exactly once.
test("teardown(false) while finalizing resolves the pending grace", async () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
    const { channel, failures } = channelUnderTest();
    void channel.prime("upload", "gmu_test");
    const worker = FakeWorker.last!;

    const finalizing = channel.teardown(true);
    expect(worker.sent).toContainEqual({ type: "stop" });
    expect(worker.terminated).toBe(0);

    await channel.teardown(false);
    await finalizing;
    expect(worker.terminated).toBe(1);
    expect(failures).toEqual([]);
  } finally {
    globalThis.Worker = realWorker;
  }
});
