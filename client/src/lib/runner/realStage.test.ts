import { expect, test } from "bun:test";
import { DEFAULT_CONFIG } from "../state/defaults";
import {
  TEST_BUILD_TOKENS,
  testHost,
  testPreparedPaths,
} from "./test-helpers.test";
import type { PhaseActivity } from "./contract";

const upload: PhaseActivity = {
  stage: "upload",
  transfer: ["up"],
  loadedLatency: false,
};
const bidi: PhaseActivity = {
  stage: "bidirectional",
  transfer: ["down", "up"],
  loadedLatency: false,
};
async function until(predicate: () => boolean): Promise<void> {
  for (let i = 0; i < 100 && !predicate(); i++) await Bun.sleep(1);
  expect(predicate()).toBe(true);
}

async function harness() {
  const originalFetch = globalThis.fetch,
    originalWorker = globalThis.Worker;
  const globals = globalThis as Record<string, unknown>;
  const originalTokens = Object.fromEntries(
    Object.keys(TEST_BUILD_TOKENS).map((key) => [
      key,
      Object.getOwnPropertyDescriptor(globals, key),
    ]),
  );
  Object.assign(globals, TEST_BUILD_TOKENS);
  const { RealBackend } = await import("./RealRunner");
  const config = structuredClone(DEFAULT_CONFIG);
  config.transferStreams = { mode: "forced", count: 1 };
  config.duration.warmupMs = 0;
  const samples: number[] = [],
    recoveryBytes: number[] = [],
    gaps: number[] = [],
    failures: {
      stage: string;
      reason: string;
      detail?: string;
      direction?: string;
    }[] = [];
  const starts: string[] = [];
  class Worker {
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: ((event: ErrorEvent) => void) | null = null;
    postMessage(message: { type: string; url?: string }) {
      if (message.type === "start") starts.push(message.url!);
    }
    terminate() {}
  }
  globalThis.Worker = Worker as unknown as typeof globalThis.Worker;
  const mints: {
    signal: AbortSignal;
    resolve: (response: Response) => void;
  }[] = [];
  const feeds = new Map<
    string,
    {
      signal: AbortSignal;
      write(record: object): boolean;
      terminal: { bytes: number; nanos: number };
    }
  >();
  const deleted: string[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input));
    if (url.pathname === "/upload/session") {
      return new Promise<Response>((resolve) =>
        mints.push({ signal: init!.signal!, resolve }),
      );
    }
    if (url.pathname !== "/upload/progress")
      throw new Error(`unexpected request ${url}`);
    const id = url.searchParams.get("id")!;
    if (init?.method === "DELETE") {
      deleted.push(id);
      const feed = feeds.get(id)!;
      feed.write({ type: "complete", ...feed.terminal });
      return new Response(null, { status: 204 });
    }
    let writer!: ReadableStreamDefaultController<Uint8Array>;
    const signal = init!.signal!;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        writer = controller;
        signal.addEventListener(
          "abort",
          () => controller.error(signal.reason),
          { once: true },
        );
      },
    });
    feeds.set(id, {
      signal,
      terminal: { bytes: 0, nanos: 0 },
      write(record) {
        if (signal.aborted) return false;
        writer.enqueue(new TextEncoder().encode(JSON.stringify(record) + "\n"));
        return true;
      },
    });
    return new Response(body);
  }) as typeof fetch;
  const backend = new RealBackend(testPreparedPaths({ latency: null }));
  backend.attach(
    testHost(config, {
      ingestThroughput(dir, bytes) {
        if (dir === "up") samples.push(bytes);
      },
      recordRecoveryBytes(_dir, bytes) {
        recoveryBytes.push(bytes);
      },
      recordRecoveryGap(_dir, seconds) {
        gaps.push(seconds);
      },
      failStage(stage, reason, detail, direction) {
        failures.push({ stage, reason, detail, direction });
      },
    }),
  );
  backend.onRunStart(config);
  return {
    backend,
    config,
    mints,
    feeds,
    starts,
    samples,
    recoveryBytes,
    gaps,
    failures,
    deleted,
    async open(index: number, id: string) {
      await until(() => mints.length > index);
      mints[index].resolve(Response.json({ uploadId: id }));
      await until(() => feeds.has(id));
      feeds.get(id)!.write({ type: "ready" });
    },
    close() {
      backend.dispose();
      globalThis.fetch = originalFetch;
      globalThis.Worker = originalWorker;
      for (const [key, descriptor] of Object.entries(originalTokens)) {
        if (descriptor) Object.defineProperty(globals, key, descriptor);
        else Reflect.deleteProperty(globals, key);
      }
    },
  };
}

test("late upload-id mint from an aborted stage cannot adopt a newer stage's resources", async () => {
  const h = await harness();
  try {
    const oldPreparation = h.backend.onStageBegin(upload);
    await until(() => h.mints.length === 1);
    h.backend.onAbort();
    expect(h.mints[0].signal.aborted).toBe(true);
    h.backend.onRunStart(h.config);
    const replacement = h.backend.onStageBegin(upload);
    await h.open(1, "new_id");
    await replacement;
    h.backend.onStageMeasure(upload);
    const current = h.feeds.get("new_id")!;
    current.write({ type: "progress", bytes: 100, nanos: 1e9 });
    current.write({ type: "progress", bytes: 200, nanos: 2e9 });
    await until(() => h.samples.length === 1);
    const starts = [...h.starts];
    h.mints[0].resolve(Response.json({ uploadId: "old_id" }));
    await oldPreparation;
    expect([...h.feeds.keys()]).toEqual(["new_id"]);
    expect(h.starts).toEqual(starts);
    expect(h.starts).toHaveLength(1);
    expect(new URL(h.starts[0]).searchParams.get("id")).toBe("new_id");
    expect(h.samples).toEqual([100]);
    expect(current.signal.aborted).toBe(false);
    current.write({ type: "progress", bytes: 350, nanos: 3e9 });
    await until(() => h.samples.length === 2);
    expect(h.samples).toEqual([100, 150]);
    expect(h.failures).toEqual([]);
  } finally {
    h.close();
  }
});

test("one upload-id rotation spans fresh stages and terminal counters remain measured", async () => {
  const h = await harness();
  const recovery = (activity: PhaseActivity) =>
    h.backend.onStageRecovery({
      stage: activity.stage,
      direction: "up",
      cause: "unknown-upload-id",
      signal: new AbortController().signal,
    });
  try {
    const initial = h.backend.onStageBegin(bidi);
    await h.open(0, "first_id");
    await initial;
    h.backend.onStageMeasure(bidi);
    const first = h.feeds.get("first_id")!;
    first.write({ type: "progress", bytes: 100, nanos: 1e9 });
    first.write({ type: "progress", bytes: 200, nanos: 2e9 });
    await until(() => h.samples.length === 1);
    const rotating = recovery(bidi);
    expect(first.signal.aborted).toBe(true);
    await h.open(1, "rotated_id");
    await rotating;
    const rotated = h.feeds.get("rotated_id")!;
    expect(first.write({ type: "progress", bytes: 99999, nanos: 3e9 })).toBe(
      false,
    );
    rotated.write({ type: "progress", bytes: 100, nanos: 1e9 });
    await until(() => h.recoveryBytes.length === 1);
    expect(h.samples).toEqual([100]);
    expect(h.recoveryBytes).toEqual([100]);
    expect(h.gaps).toHaveLength(1);
    expect(h.gaps[0]).toBeGreaterThanOrEqual(0);
    rotated.write({ type: "progress", bytes: 250, nanos: 2e9 });
    await until(() => h.samples.length === 2);
    expect(h.samples).toEqual([100, 150]);
    await recovery(bidi);
    expect(h.mints).toHaveLength(2);
    rotated.terminal = { bytes: 400, nanos: 3e9 };
    await h.backend.onStageEnd(bidi);
    expect(h.samples).toEqual([100, 150, 150]);
    expect(h.deleted).toEqual(["rotated_id"]);
    const later = h.backend.onStageBegin(upload);
    await h.open(2, "later_id");
    await later;
    h.backend.onStageMeasure(upload);
    await recovery(upload);
    expect(h.mints).toHaveLength(3);
    expect(h.feeds.get("later_id")!.signal.aborted).toBe(false);
    expect(h.recoveryBytes).toEqual([100]);
    expect(h.gaps).toHaveLength(1);
    expect(h.failures).toEqual([]);
  } finally {
    h.close();
  }
});

test("bidirectional upload mint failure reports the failed upload despite an active download", async () => {
  const h = await harness();
  try {
    const preparing = h.backend.onStageBegin(bidi);
    await until(() => h.mints.length === 1);
    expect(h.starts).toHaveLength(1);
    expect(new URL(h.starts[0]).pathname).toBe("/download");
    h.mints[0].resolve(new Response(null, { status: 500 }));
    await preparing;
    expect(h.failures).toEqual([
      {
        stage: "bidirectional",
        reason: "protocol-error",
        direction: "up",
        detail: "upload session could not be established",
      },
    ]);
    expect(h.starts).toHaveLength(1);
    expect(h.feeds.size).toBe(0);
    expect(h.samples).toEqual([]);
  } finally {
    h.close();
  }
});
