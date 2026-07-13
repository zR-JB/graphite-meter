import { test, expect } from "bun:test";
import type { CoreHost } from "./core";
import {
  resolveBase,
  httpToWs,
  wsToWss,
  median,
  needsPings,
  laneStaggerMs,
  selectProtocolTarget,
  browserProtocolMatchesTarget,
  protocolTargetKey,
} from "./real/backendPure";
import type { PhaseActivity, RunnerConfig } from "./contract";

const routes = {
  probe: "/probe",
  download: "/download",
  upload: "/upload",
  uploadSession: "/upload/session",
  websocket: null,
  webtransport: null,
};

test("selectProtocolTarget freezes the requested advertised target", () => {
  const targets = {
    http1: { origin: "http://meter:8765", routes },
    http2: { origin: "https://meter:8443", routes },
    http3: null,
  };
  expect(
    selectProtocolTarget(targets, "http2", "http://meter:8765", false)
      ?.protocol,
  ).toBe("http2");
  expect(
    selectProtocolTarget(targets, "http3", "http://meter:8765", false),
  ).toBeNull();
});

test("selectProtocolTarget rejects clear H1 on a secure page", () => {
  const targets = {
    http1: { origin: "http://meter:8765", routes },
    http2: null,
    http3: null,
  };
  expect(
    selectProtocolTarget(targets, "http1", "https://meter", true),
  ).toBeNull();
});

test("current target follows the negotiated protocol on a shared origin", () => {
  const shared = "https://meter";
  const targets = {
    http1: null,
    http2: { origin: shared, routes },
    http3: { origin: shared, routes },
  };
  expect(
    selectProtocolTarget(targets, "current", shared, true, "h2")?.protocol,
  ).toBe("http2");
  expect(
    selectProtocolTarget(targets, "current", shared, true, "h3")?.protocol,
  ).toBe("http3");
});

test("browser protocol verification is independent of server probe evidence", () => {
  expect(browserProtocolMatchesTarget("http1", "http/1.1")).toBe(true);
  expect(browserProtocolMatchesTarget("http2", "h2")).toBe(true);
  expect(browserProtocolMatchesTarget("http3", "h3")).toBe(true);
  expect(browserProtocolMatchesTarget("http2", "http/1.1")).toBe(false);
});

test("idle target ownership includes protocol and public origin", () => {
  const target = { origin: "https://meter", routes };
  expect(protocolTargetKey("http2", target)).toBe("http2\nhttps://meter");
  expect(protocolTargetKey("http3", target)).not.toBe(
    protocolTargetKey("http2", target),
  );
  expect(
    protocolTargetKey("http2", { ...target, origin: "https://other-meter" }),
  ).not.toBe(protocolTargetKey("http2", target));
});

/* ---------- resolveBase ---------- */

test("resolveBase: undefined/auto/empty host all mean same-origin (relative)", () => {
  expect(resolveBase(undefined)).toBe("");
  expect(resolveBase({ host: "auto", port: 8765 })).toBe("");
  expect(resolveBase({ host: "", port: 8765 })).toBe("");
});

test("resolveBase: builds an absolute http origin for a concrete host", () => {
  expect(resolveBase({ host: "example.com", port: 8765 })).toBe(
    "http://example.com:8765",
  );
});

test("resolveBase: port 443 builds an https origin", () => {
  expect(resolveBase({ host: "example.com", port: 443 })).toBe(
    "https://example.com:443",
  );
});

/* ---------- httpToWs ---------- */

test("httpToWs: maps https:// to wss:// and http:// to ws://", () => {
  expect(httpToWs("https://example.com:443")).toBe("wss://example.com:443");
  expect(httpToWs("http://example.com:8765")).toBe("ws://example.com:8765");
});

test("httpToWs: passes through anything already ws(s):// or relative", () => {
  expect(httpToWs("wss://example.com")).toBe("wss://example.com");
  expect(httpToWs("ws://example.com")).toBe("ws://example.com");
  expect(httpToWs("")).toBe("");
});

/* ---------- wsToWss ---------- */

test("wsToWss: upgrades ws:// to wss://", () => {
  expect(wsToWss("ws://example.com:8765")).toBe("wss://example.com:8765");
});

test("wsToWss: leaves wss:// (or anything else) unchanged", () => {
  expect(wsToWss("wss://example.com:443")).toBe("wss://example.com:443");
  expect(wsToWss("")).toBe("");
});

/* ---------- median ---------- */

test("median: odd-length list returns the middle value", () => {
  expect(median([3, 1, 2])).toBe(2);
});

test("median: even-length list averages the two middle values", () => {
  expect(median([4, 1, 3, 2])).toBe(2.5);
});

test("median: single-element list returns that element", () => {
  expect(median([7])).toBe(7);
});

test("median: does not mutate the input array", () => {
  const xs = [3, 1, 2];
  median(xs);
  expect(xs).toEqual([3, 1, 2]);
});

/* ---------- needsPings ---------- */

const activity = (overrides: Partial<PhaseActivity> = {}): PhaseActivity => ({
  stage: "download",
  transfer: ["down"],
  loadedLatency: false,
  ...overrides,
});

test("needsPings: the latency stage always needs pings", () => {
  expect(
    needsPings(
      activity({ stage: "latency", transfer: [], loadedLatency: false }),
    ),
  ).toBe(true);
});

test("needsPings: a transfer stage needs pings only when loadedLatency is on", () => {
  expect(needsPings(activity({ loadedLatency: true }))).toBe(true);
  expect(needsPings(activity({ loadedLatency: false }))).toBe(false);
});

test("needsPings: loadedLatency alone is not enough without transfer lanes", () => {
  expect(
    needsPings(
      activity({ transfer: [], loadedLatency: true, stage: "download" }),
    ),
  ).toBe(false);
});

/* ---------- laneStaggerMs ---------- */

test("laneStaggerMs: a single lane (or fewer) never staggers", () => {
  expect(laneStaggerMs(1, 4000, 75)).toBe(0);
  expect(laneStaggerMs(0, 4000, 75)).toBe(0);
});

test("laneStaggerMs: zero warmup means lanes spawn together", () => {
  expect(laneStaggerMs(4, 0, 75)).toBe(0);
});

test("laneStaggerMs: splits half the warmup window across the non-first lanes", () => {
  // 4 lanes -> 3 gaps; half of a 3000ms warmup (1500ms) split 3 ways = 500ms/gap.
  expect(laneStaggerMs(4, 3000, 500)).toBe(500);
});

test("laneStaggerMs: caps at the base stagger even on a long warmup", () => {
  // A generous warmup shouldn't stretch the per-lane gap past the base.
  expect(laneStaggerMs(2, 100_000, 75)).toBe(75);
});

test("each probe refreshes discovery and upload progress opens before forced H1 lanes", async () => {
  const buildGlobals = globalThis as typeof globalThis &
    Record<string, unknown>;
  Object.assign(buildGlobals, {
    __GM_DEFAULT_ENGINE__: "real",
    __GM_ALLOW_DUMMY__: false,
    __GM_DEV_TOOLS__: false,
    __GM_BUILD_LABEL__: "test",
    __GM_CLIENT_VERSION__: "0.0.0-test",
  });
  const { RealBackend } = await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realWorker = globalThis.Worker;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const started: string[] = [];
  let preflights = 0;
  let progressWorker: FakeWorker | null = null;
  let pingWorker: FakeWorker | null = null;

  class FakeWorker {
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: ((event: ErrorEvent) => void) | null = null;
    readonly kind: "ping" | "progress" | "upload" | "download";

    constructor(url: URL) {
      const path = String(url);
      this.kind = path.includes("upload-progress")
        ? "progress"
        : path.includes("upload-worker")
          ? "upload"
          : path.includes("download-worker")
            ? "download"
            : "ping";
      if (this.kind === "ping") pingWorker = this;
    }

    postMessage(message: { type: string }): void {
      if (message.type !== "start" && message.type !== "measure") return;
      if (this.kind === "ping") {
        queueMicrotask(() =>
          this.emit({
            type: "samples",
            samples: [
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
            ],
          }),
        );
      } else if (message.type === "start") {
        started.push(this.kind);
        if (this.kind === "progress") progressWorker = this;
      }
    }

    emit(data: unknown): void {
      this.onmessage?.({ data } as MessageEvent);
    }

    terminate(): void {}
  }

  const targetRoutes = {
    ...routes,
    websocket: { ping: "/ws/ping", uploadProgress: "/ws/upload" },
  };
  const discovery = (withH2: boolean) => ({
    server: { name: "test", host: "meter.test", port: 8765 },
    engineVersion: "test",
    capabilities: {
      targets: {
        http1: { origin: "http://meter.test:8765", routes: targetRoutes },
        http2: withH2
          ? { origin: "https://meter.test:8443", routes: targetRoutes }
          : null,
        http3: null,
      },
    },
  });

  try {
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL("http://meter.test:8765/"),
    });
    globalThis.Worker = FakeWorker as unknown as typeof Worker;
    performance.getEntriesByName = () =>
      [{ nextHopProtocol: "http/1.1" }] as unknown as PerformanceEntry[];
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) {
        preflights++;
        return Response.json(discovery(preflights > 1));
      }
      if (url.includes("/probe"))
        return Response.json({
          clientIp: "127.0.0.1",
          clientIpVersion: 4,
          clientIpSource: "socket",
          protocolNegotiated: "http/1.1",
        });
      if (url.includes("/upload/session"))
        return Response.json({ uploadId: "gmu_test" });
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;

    const config = {
      endpoint: { host: "meter.test", port: 8765, protocol: "http1" },
      transferStreams: { mode: "forced", count: 6 },
      duration: { warmupMs: 0 },
      pingConcurrency: "medium",
      experimentalChunkedDownload: false,
    } as RunnerConfig;
    const failures: string[] = [];
    const host = {
      config,
      phase: "idle",
      elapsed: 0,
      emit() {},
      reportTransport() {},
      fail() {},
      failStage(_stage, _reason, message) {
        failures.push(message);
      },
      ingestThroughput() {},
      ingestLatency() {},
      stall() {},
      resume() {},
    } as CoreHost;
    const backend = new RealBackend();
    backend.attach(host);

    const first = await backend.probe(config.endpoint);
    const secondProbe = backend.probe(config.endpoint);
    for (let i = 0; i < 10; i++) await Promise.resolve();
    pingWorker!.emit({
      type: "samples",
      samples: Array.from({ length: 5 }, () => ({ rtt: 1, lost: false })),
    });
    const second = await secondProbe;
    expect(preflights).toBe(2);
    expect(first.availableTargets?.http2).toBe(false);
    expect(second.availableTargets?.http2).toBe(true);

    backend.onRunStart(config);
    const preparation = backend.onStageBegin({
      stage: "upload",
      transfer: ["up"],
      loadedLatency: false,
    });
    for (let i = 0; i < 10 && !progressWorker; i++) await Promise.resolve();
    expect(started).toEqual(["progress"]);

    progressWorker!.emit({ type: "open" });
    await preparation;
    expect(started).toEqual([
      "progress",
      "upload",
      "upload",
      "upload",
      "upload",
      "upload",
      "upload",
    ]);
    expect(failures).toEqual([]);
    backend.onAbort();
  } finally {
    globalThis.fetch = realFetch;
    globalThis.Worker = realWorker;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
    else Reflect.deleteProperty(globalThis, "location");
    performance.getEntriesByName = realEntries;
    for (const key of [
      "__GM_DEFAULT_ENGINE__",
      "__GM_ALLOW_DUMMY__",
      "__GM_DEV_TOOLS__",
      "__GM_BUILD_LABEL__",
      "__GM_CLIENT_VERSION__",
    ])
      Reflect.deleteProperty(buildGlobals, key);
  }
});
