import { test, expect } from "bun:test";
import { emptyConnectionValidation } from "./connectionModel";
import type { PhaseActivity, RunnerConfig } from "./contract";
import {
  TEST_BUILD_TOKENS,
  TEST_WT_ORIGIN,
  TEST_WT_PREFLIGHT,
  testHost,
  testWtConfig,
} from "./test-helpers.test";
const WT_ORIGIN = TEST_WT_ORIGIN;
class LiveWebTransport {
  readonly ready = Promise.resolve();
  readonly closed = new Promise<void>(() => {});
  readonly incomingUnidirectionalStreams = new ReadableStream({
    start(controller) {
      controller.enqueue(
        new ReadableStream({
          start(lane) {
            lane.enqueue(new Uint8Array(1024));
            lane.close();
          },
        }),
      );
      controller.close();
    },
  });
  close(): void {}
}
interface Sent {
  type: string;
  url?: string;
  seq?: number;
}
class FakeWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  readonly kind: "session" | "ping" | "other";
  readonly sent: Sent[] = [];
  constructor(url: URL) {
    const path = String(url);
    this.kind = path.includes("wt-transfer-worker")
      ? "session"
      : path.includes("ping-worker")
        ? "ping"
        : "other";
    if (this.kind === "session") sessions.push(this);
  }
  postMessage(message: Sent): void {
    this.sent.push(message);
    if (this.kind === "ping" && message.type === "start") {
      queueMicrotask(() => {
        this.emit({ type: "ready" });
        this.emit({
          type: "samples",
          samples: Array.from({ length: 5 }, () => ({ rtt: 1, lost: false })),
        });
      });
      return;
    }
    if (this.kind !== "session") return;
    if (message.type === "start")
      queueMicrotask(() => this.emit({ type: "established" }));
    if (message.type === "stop")
      queueMicrotask(() => this.emit({ type: "stopped" }));
  }
  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
  terminate(): void {}
}
let sessions: FakeWorker[] = [];
const preflight = TEST_WT_PREFLIGHT;
function baseConfig(): RunnerConfig {
  return testWtConfig();
}
function activity(stage: "download" | "upload"): PhaseActivity {
  return {
    stage,
    transfer: [stage === "download" ? "down" : "up"],
    loadedLatency: false,
  };
}
interface Harness {
  backend: import("./RealRunner").RealBackend;
  throughput: { dir: string; bytes: number }[];
  failures: string[];
  session(): FakeWorker;
}
async function withBackend(body: (h: Harness) => Promise<void>): Promise<void> {
  const globals = globalThis as Record<string, unknown>;
  Object.assign(globals, TEST_BUILD_TOKENS);
  const { RealBackend } = await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realWorker = globalThis.Worker;
  const realWebTransport = globals.WebTransport;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  sessions = [];
  try {
    globals.WebTransport = LiveWebTransport;
    globalThis.Worker = FakeWorker as unknown as typeof Worker;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL(`${WT_ORIGIN}/`),
    });
    performance.getEntriesByName = () =>
      [{ nextHopProtocol: "h3" }] as unknown as PerformanceEntry[];
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(preflight);
      if (url.includes("/probe"))
        return Response.json({
          clientIp: "127.0.0.1",
          clientIpVersion: 4,
          clientIpSource: "socket",
          protocolNegotiated: "h3",
        });
      if (url.includes("/upload/session"))
        return Response.json({ uploadId: "gmu_test" });
      if (url.includes("/upload/progress")) return new Response(null);
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;
    const config = baseConfig();
    const throughput: { dir: string; bytes: number }[] = [];
    const failures: string[] = [];
    const host = testHost(config, {
      fail(_reason: string, message: string) {
        failures.push(message);
      },
      failStage(_stage: string, _reason: string, message: string) {
        failures.push(message);
      },
      ingestThroughput(dir: string, bytes: number) {
        throughput.push({ dir, bytes });
      },
    });
    const { prepareConnections } = await import("./real/prepare");
    const prepared = await prepareConnections(
      config,
      emptyConnectionValidation(),
      ["throughput", "latency"],
      new AbortController().signal,
    );
    prepared.idle?.stop();
    expect(prepared.failure).toBeUndefined();
    const paths = {
      discovery: prepared.discovery,
      throughput: prepared.validation.throughput.path!,
      latency: prepared.validation.latency.path,
    };
    expect(paths.throughput.target.transport).toBe("webtransport");
    const backend = new RealBackend(paths);
    backend.attach(host);
    backend.onRunStart(config);
    await body({
      backend,
      throughput,
      failures,
      session: () => sessions.at(-1)!,
    });
  } finally {
    globalThis.fetch = realFetch;
    globalThis.Worker = realWorker;
    performance.getEntriesByName = realEntries;
    if (realWebTransport === undefined) delete globals.WebTransport;
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
    for (const key of [
      "__GM_ALLOW_DUMMY__",
      "__GM_BUILD_PROFILE__",
      "__GM_RELEASE_VERSION__",
      "__GM_SOURCE_REVISION__",
      "__GM_BUILD_IDENTITY__",
      "__GM_CLIENT_VERSION__",
    ])
      Reflect.deleteProperty(globals, key);
  }
}
test("WebTransport download carries bytes and upload is metered by the server feed", async () => {
  await withBackend(async ({ backend, throughput, failures, session }) => {
    const phase = activity("download");
    await backend.onStageBegin(phase);
    expect(sessions).toHaveLength(1);
    const start = session().sent[0];
    expect(start.url).toBe(
      `${WT_ORIGIN}/wt/download?bytes=68719476736&streams=4`,
    );
    session().emit({ type: "progress", bytes: 999, elapsedMs: 10, seq: 0 });
    backend.onStageMeasure(phase);
    expect(session().sent.at(-1)).toEqual({ type: "measure", seq: 1 });
    session().emit({
      type: "progress",
      bytes: 4_000_000,
      elapsedMs: 50,
      seq: 1,
    });
    await Bun.sleep(5);
    await backend.onStageEnd(phase);
    expect(throughput).toEqual([{ dir: "down", bytes: 4_000_000 }]);
    expect(failures).toEqual([]);
    expect(session().sent.map((m) => m.type)).toContain("stop");
    const uploadPhase = activity("upload");
    const beginning = backend.onStageBegin(uploadPhase);
    for (let i = 0; i < 20 && sessions.length < 2; i++) await Promise.resolve();
    expect(session().sent[0].url).toBe(`${WT_ORIGIN}/wt/upload?id=gmu_test`);
    session().emit({ type: "upload-progress", msg: { type: "open" } });
    await beginning;
    expect(failures).toEqual([]);
    backend.onStageMeasure(uploadPhase);
    for (const n of [100, 250])
      session().emit({
        type: "upload-progress",
        msg: { type: "bytes", n, t: n * 1_000_000 },
      });
    const ending = backend.onStageEnd(uploadPhase);
    session().emit({
      type: "upload-progress",
      msg: { type: "complete", n: 400, t: 400_000_000 },
    });
    await ending;
    expect(throughput.map((s) => s.dir)).toEqual(["down", "up", "up"]);
    expect(throughput.map((s) => s.bytes)).toEqual([4_000_000, 150, 150]);
    expect(failures).toEqual([]);
  });
});
