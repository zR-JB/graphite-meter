import { test, expect } from "bun:test";
import type { CoreHost } from "./core";
import type { PhaseActivity, RunnerConfig } from "./contract";

const WT_ORIGIN = "https://meter.test";

/** A path that establishes and delivers one lane, which is what the throughput
 *  check requires before it reports Ready. */
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

/** Speaks the worker protocol without running one: the session worker is the
 *  only thing between a lane and the network. */
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

const preflight = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [
      { baseUrl: WT_ORIGIN, transport: "webtransport", protocol: "http3" },
    ],
    latency: [{ baseUrl: WT_ORIGIN, transport: "websocket" }],
  },
};

function baseConfig(): RunnerConfig {
  return {
    stages: {
      latency: false,
      download: true,
      upload: true,
      bidirectional: false,
    },
    skipLoadedLatencyWhenStageOff: true,
    transports: {
      throughputTarget: `${WT_ORIGIN}::wt`,
      latencyTarget: "auto",
    },
    transferStreams: { mode: "forced", count: 4 },
    duration: {
      warmupMs: 0,
      latencyMs: 1,
      downloadMs: 1,
      uploadMs: 1,
      bidirectionalMs: 1,
    },
    pingCadence: "reply-driven",
    loadedPingCadence: "medium",
    experimentalChunkedDownload: false,
    experimentalDatagramThroughput: false,
    compensation: {
      profile: "loopback",
      transport: "auto",
      params: {
        mtuBytes: 65536,
        ipVersion: "auto",
        vlanTagged: false,
        tcpOptionsMinBytes: 0,
        tcpOptionsMaxBytes: 0,
        encapsulationBytes: 0,
        quicConnIdMinBytes: 0,
        quicConnIdMaxBytes: 0,
      },
    },
    adaptive: {
      enabled: false,
      minCoverageRatio: 1,
      stabilityThreshold: 1,
      maxPhaseReductionRatio: 0,
      minLatencySamples: 1,
      minTransferSamples: 1,
      confirmationMs: 0,
    },
    visualization: { throughputMaxBytesPerSec: "auto" },
  };
}

interface Harness {
  backend: import("./RealRunner").RealBackend;
  throughput: { dir: string; bytes: number }[];
  failures: string[];
  session(): FakeWorker;
}

/** Boots a real backend against a WebTransport-only origin, probed and ready to
 *  run a stage. Restores every global it replaced. */
async function withBackend(body: (h: Harness) => Promise<void>): Promise<void> {
  const globals = globalThis as Record<string, unknown>;
  Object.assign(globals, {
    __GM_DEFAULT_ENGINE__: "real",
    __GM_ALLOW_DUMMY__: false,
    __GM_DEV_TOOLS__: false,
    __GM_BUILD_LABEL__: "test",
    __GM_CLIENT_VERSION__: "0.0.0-test",
  });
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
    const host = {
      config,
      phase: "idle",
      elapsed: 0,
      emit() {},
      fail(_reason: string, message: string) {
        failures.push(message);
      },
      failStage(_stage: string, _reason: string, message: string) {
        failures.push(message);
      },
      ingestThroughput(dir: string, _rate: number, bytes: number) {
        throughput.push({ dir, bytes });
      },
      ingestLatency() {},
      recordRecoveryGap() {},
      presentationRate() {
        return 0;
      },
      stall() {},
      resume() {},
    } as unknown as CoreHost;

    const backend = new RealBackend();
    backend.attach(host);
    const probe = backend.probe(config);
    for (let i = 0; i < 20; i++) await Promise.resolve();
    const info = await probe;
    expect(info.selectedThroughputTransport).toBe("webtransport");
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
      "__GM_DEFAULT_ENGINE__",
      "__GM_ALLOW_DUMMY__",
      "__GM_DEV_TOOLS__",
      "__GM_BUILD_LABEL__",
      "__GM_CLIENT_VERSION__",
    ])
      Reflect.deleteProperty(globals, key);
  }
}

// One session carries every lane, so the stage opens one worker whatever the
// stream count, and only reports that carry the live measure epoch are counted.
test("a WebTransport download stage carries bytes into the core", async () => {
  await withBackend(async ({ backend, throughput, failures, session }) => {
    const activity: PhaseActivity = {
      stage: "download",
      transfer: ["down"],
      loadedLatency: false,
    };
    await backend.onStageBegin(activity);
    expect(sessions).toHaveLength(1);
    const start = session().sent[0];
    expect(start.url).toBe(
      `${WT_ORIGIN}/wt/download?bytes=68719476736&streams=4`,
    );

    session().emit({ type: "progress", bytes: 999, elapsedMs: 10, seq: 0 });
    backend.onStageMeasure(activity);
    expect(session().sent.at(-1)).toEqual({ type: "measure", seq: 1 });

    session().emit({
      type: "progress",
      bytes: 4_000_000,
      elapsedMs: 50,
      seq: 1,
    });
    await Bun.sleep(5);
    await backend.onStageEnd(activity);

    expect(throughput).toEqual([{ dir: "down", bytes: 4_000_000 }]);
    expect(failures).toEqual([]);
    expect(session().sent.map((m) => m.type)).toContain("stop");
  });
});

// Upload is metered by the server's feed, which rides the same session. The
// stage may not open its lanes before the feed is running, or the first bytes
// go uncounted.
test("a WebTransport upload stage is metered by the server feed", async () => {
  await withBackend(async ({ backend, throughput, failures, session }) => {
    const activity: PhaseActivity = {
      stage: "upload",
      transfer: ["up"],
      loadedLatency: false,
    };
    const beginning = backend.onStageBegin(activity);
    for (let i = 0; i < 20 && sessions.length === 0; i++)
      await Promise.resolve();
    expect(session().sent[0].url).toBe(`${WT_ORIGIN}/wt/upload?id=gmu_test`);

    session().emit({ type: "upload-progress", msg: { type: "open" } });
    await beginning;
    expect(failures).toEqual([]);

    backend.onStageMeasure(activity);
    for (const n of [100, 250])
      session().emit({
        type: "upload-progress",
        msg: { type: "bytes", n, t: n * 1_000_000 },
      });

    const ending = backend.onStageEnd(activity);
    session().emit({
      type: "upload-progress",
      msg: { type: "complete", n: 400, t: 400_000_000 },
    });
    await ending;

    // The first record after measure is the baseline, and every later one is a
    // delta against the server's running total.
    expect(throughput.map((s) => s.dir)).toEqual(["up", "up"]);
    expect(throughput.map((s) => s.bytes)).toEqual([150, 150]);
    expect(failures).toEqual([]);
  });
});
