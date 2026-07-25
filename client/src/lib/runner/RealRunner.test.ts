import { test, expect } from "bun:test";
import type { CoreHost } from "./core";
import {
  httpToWs,
  throughputRidesSession,
  median,
  needsPings,
  laneStaggerMs,
  protocolFromNextHop,
  selectThroughputTarget,
  selectLatencyTarget,
  browserProtocolMatchesTarget,
  classifyTransportDiscovery,
  isLoopbackHostname,
  throughputTargetKey,
  fetchViewOfWebTransport,
  ROUTES,
} from "./real/backendPure";
import type { PhaseActivity, RunnerConfig } from "./contract";
import type { FetchThroughputTarget, LatencyTarget } from "../api/endpoints";

const routes = {
  probe: ROUTES.probe,
  download: ROUTES.download,
  upload: ROUTES.upload,
  uploadSession: ROUTES.uploadSession,
  uploadProgress: ROUTES.uploadProgress,
};

const transfer = (
  id: string,
  origin: string,
  protocol: FetchThroughputTarget["protocol"],
  tls: boolean,
): FetchThroughputTarget => ({
  id,
  origin,
  transport: "fetch-stream",
  protocol,
  tls,
  routes,
});

const discovery = (
  throughput: FetchThroughputTarget[],
  latency: LatencyTarget[] = [],
  pageOrigin = "http://meter:7246",
  pageSecure = false,
  pageProtocol = "http/1.1",
) =>
  classifyTransportDiscovery(
    throughput,
    latency,
    pageOrigin,
    pageSecure,
    pageProtocol,
  );

test("proxy endpoints resolve relative to preflight and negotiate the browser hop", () => {
  const catalog = classifyTransportDiscovery(
    [{ baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" }],
    [{ baseUrl: ".", transport: "websocket" }],
    "https://meter.example",
    true,
    "h2",
  );
  const target = selectThroughputTarget(catalog, "auto");
  expect(target?.origin).toBe("https://meter.example");
  expect(target?.protocol).toBe("negotiated");
  if (target?.transport !== "fetch-stream") throw new Error("not fetch");
  expect(browserProtocolMatchesTarget(target, "h2")).toBe(true);
  expect(selectLatencyTarget(catalog, "auto")?.origin).toBe(
    "https://meter.example",
  );
});

test("deterministic native target wins when self resolves to the same origin", () => {
  const catalog = classifyTransportDiscovery(
    [
      {
        baseUrl: "https://meter.example",
        transport: "fetch-stream",
        protocol: "http1",
      },
      { baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" },
    ],
    [],
    "https://meter.example",
    true,
    "http/1.1",
  );
  expect(catalog.throughput["https://meter.example"].target?.protocol).toBe(
    "http1",
  );
  expect(selectThroughputTarget(catalog, "auto")?.protocol).toBe("http1");
});

test("native endpoints remain deterministic and mixed content stays blocked", () => {
  const catalog = classifyTransportDiscovery(
    [
      {
        baseUrl: "http://meter:7246",
        transport: "fetch-stream",
        protocol: "http1",
      },
      {
        baseUrl: "https://meter:7248",
        transport: "fetch-stream",
        protocol: "http2",
      },
    ],
    [],
    "https://ui.example",
    true,
    "h2",
  );
  expect(catalog.throughput["http://meter:7246"].state).toBe("browser-blocked");
  expect(selectThroughputTarget(catalog, "https://meter:7248")?.protocol).toBe(
    "http2",
  );
  const h2Target = selectThroughputTarget(catalog, "https://meter:7248");
  if (h2Target?.transport !== "fetch-stream") throw new Error("not fetch");
  expect(browserProtocolMatchesTarget(h2Target, "http/1.1")).toBe(false);
});

test("WebTransport folds onto its origin and leads latency auto-selection", () => {
  const catalog = classifyTransportDiscovery(
    [
      {
        baseUrl: "https://meter:7249",
        transport: "fetch-stream",
        protocol: "http3",
      },
      {
        baseUrl: "https://meter:7249",
        transport: "webtransport",
        protocol: "http3",
      },
      {
        baseUrl: "https://meter:7249",
        transport: "webtransport-datagram",
        protocol: "http3",
      },
    ],
    [
      { baseUrl: "https://meter:7247", transport: "websocket" },
      { baseUrl: "https://meter:7249", transport: "webtransport" },
    ],
    "https://meter:7249",
    true,
    "h3",
  );
  const entry = catalog.throughput["https://meter:7249"];
  expect(entry.target?.transport).toBe("fetch-stream");
  expect(entry.wt?.id).toBe("https://meter:7249::wt");
  expect(entry.wt?.routes.wtDownload).toBe(ROUTES.wtDownload);
  // The datagram path is its own advertised view, never folded onto ::wt.
  expect(entry.wtDatagram?.id).toBe("https://meter:7249::wtdg");
  expect(entry.wtDatagram?.transport).toBe("webtransport-datagram");

  // Auto prefers fetch; the ::wt id names the WebTransport view explicitly.
  expect(selectThroughputTarget(catalog, "auto")?.transport).toBe(
    "fetch-stream",
  );
  expect(
    selectThroughputTarget(catalog, "https://meter:7249::wt")?.transport,
  ).toBe("webtransport");
  expect(
    selectThroughputTarget(catalog, "https://meter:7249::wtdg")?.transport,
  ).toBe("webtransport-datagram");
  expect(selectThroughputTarget(catalog, "https://meter:7249")?.transport).toBe(
    "fetch-stream",
  );

  // A client that cannot drive WebTransport never selects it.
  expect(selectLatencyTarget(catalog, "auto")?.transport).toBe("websocket");
  expect(selectLatencyTarget(catalog, "https://meter:7249", false)).toBeNull();
  const wt = selectLatencyTarget(catalog, "auto", true);
  expect(wt?.transport).toBe("webtransport");
  expect(wt?.origin).toBe("https://meter:7249");
});

// A proxy serving TCP and UDP on one hostname advertises both latency buses on
// one origin. Keeping only the WebTransport view would leave a UDP-blocked or
// WebTransport-less client with no latency target at all.
test("one origin advertising both latency buses keeps the WebSocket fallback", () => {
  const catalog = classifyTransportDiscovery(
    [
      {
        baseUrl: "https://meter",
        transport: "fetch-stream",
        protocol: "http3",
      },
    ],
    [
      { baseUrl: "https://meter", transport: "websocket" },
      { baseUrl: "https://meter", transport: "webtransport" },
    ],
    "https://meter",
    true,
    "h3",
  );
  const entry = catalog.latency["https://meter"];
  expect(entry.target?.transport).toBe("websocket");
  expect(entry.wt?.transport).toBe("webtransport");
  expect(entry.wt?.id).toBe("https://meter::wt");

  // Auto prefers the datagram bus where it runs, and degrades to WebSocket on
  // a client that cannot drive it — the UDP-blocked fallback.
  expect(selectLatencyTarget(catalog, "auto", true)?.transport).toBe(
    "webtransport",
  );
  expect(selectLatencyTarget(catalog, "auto", false)?.transport).toBe(
    "websocket",
  );
  // Explicit ids name a bus each; the plain origin is the WebSocket view.
  expect(
    selectLatencyTarget(catalog, "https://meter::wt", true)?.transport,
  ).toBe("webtransport");
  expect(selectLatencyTarget(catalog, "https://meter", false)?.transport).toBe(
    "websocket",
  );
});

test("a WebTransport-only origin is auto's last resort and keeps a fetch view", () => {
  const catalog = classifyTransportDiscovery(
    [
      {
        baseUrl: "https://meter:7249",
        transport: "webtransport",
        protocol: "http3",
      },
    ],
    [],
    "https://ui.example",
    true,
    "h3",
  );
  const target = selectThroughputTarget(catalog, "auto");
  expect(target?.transport).toBe("webtransport");
  if (target?.transport !== "webtransport") throw new Error("not wt");
  const view = fetchViewOfWebTransport(target);
  expect(view.transport).toBe("fetch-stream");
  expect(view.origin).toBe("https://meter:7249");
  expect(view.routes.uploadSession).toBe(ROUTES.uploadSession);
});

test("browser protocol verification is independent of server probe evidence", () => {
  const h1 = transfer("http1-tls", "https://meter", "http1", true);
  const h2 = transfer("http2", "https://meter", "http2", true);
  const negotiated = transfer(
    "https://meter",
    "https://meter",
    "negotiated",
    true,
  );
  expect(browserProtocolMatchesTarget(h1, "http/1.1")).toBe(true);
  expect(browserProtocolMatchesTarget(h2, "h2")).toBe(true);
  expect(browserProtocolMatchesTarget(h2, "http/1.1")).toBe(false);
  expect(browserProtocolMatchesTarget(negotiated)).toBe(true);
  expect(browserProtocolMatchesTarget(h2)).toBe(false);
  expect(protocolFromNextHop()).toBeUndefined();
  expect(protocolFromNextHop("h2")).toBe("http2");
});

test("idle target ownership includes protocol and public origin", () => {
  const target = transfer("http2", "https://meter", "http2", true);
  expect(throughputTargetKey(target)).toBe("http2\nhttps://meter");
  expect(
    throughputTargetKey({ ...target, origin: "https://other-meter" }),
  ).not.toBe(throughputTargetKey(target));
});

test("clear loopback targets stay usable from HTTPS", () => {
  for (const host of ["localhost", "meter.localhost", "127.42.0.9", "[::1]"]) {
    const target = transfer(
      "http1-clear",
      `http://${host}:7246`,
      "http1",
      false,
    );
    expect(
      discovery([target], [], "https://ui.example", true).throughput[
        target.origin
      ].state,
    ).toBe("advertised");
  }
  expect(isLoopbackHostname("127.255.1.2")).toBe(true);
});

/* ---------- httpToWs ---------- */

test("httpToWs: maps https:// to wss:// and http:// to ws://", () => {
  expect(httpToWs("https://example.com:443")).toBe("wss://example.com:443");
  expect(httpToWs("http://example.com:7246")).toBe("ws://example.com:7246");
});

test("httpToWs: passes through anything already ws(s):// or relative", () => {
  expect(httpToWs("wss://example.com")).toBe("wss://example.com");
  expect(httpToWs("ws://example.com")).toBe("ws://example.com");
  expect(httpToWs("")).toBe("");
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

// One test, because every assertion below depends on backend state the previous
// probes leave behind: frozen role targets, remembered protocol, discovery
// generation. Replaying that setup per case does not reproduce it.
test("real backend: probe refresh keeps the negotiated protocol per role, and the upload stage opens its progress channel before any POST lane", async () => {
  const buildGlobals = globalThis as typeof globalThis &
    Record<string, unknown>;
  Object.assign(buildGlobals, {
    __GM_DEFAULT_ENGINE__: "real",
    __GM_ALLOW_DUMMY__: false,
    __GM_DEV_TOOLS__: false,
    __GM_BUILD_LABEL__: "test",
    __GM_CLIENT_VERSION__: "0.0.0-test",
  });
  const { RealBackend, TransportUnavailableError } =
    await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realWorker = globalThis.Worker;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const started: string[] = [];
  const workerStarts: { kind: string; url: string }[] = [];
  const pingMessages: Record<string, unknown>[] = [];
  const fetchUrls: string[] = [];
  let preflights = 0;
  let browserProtocol = "http/1.1";
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

    postMessage(
      message: { type: string; url?: string } & Record<string, unknown>,
    ): void {
      if (this.kind === "ping") pingMessages.push(message);
      if (message.type !== "start" && message.type !== "measure") return;
      if (message.type === "start" && message.url)
        workerStarts.push({ kind: this.kind, url: message.url });
      if (this.kind === "ping") {
        queueMicrotask(() => {
          this.emit({ type: "ready" });
          this.emit({
            type: "samples",
            samples: [
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
              { rtt: 1, lost: false },
            ],
          });
        });
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

  const discovery = (withH2: boolean) => ({
    server: { name: "test" },
    engineVersion: "test",
    generation: withH2 ? "b" : "a",
    capabilities: {
      throughput: [
        {
          baseUrl: "http://meter.test:7246",
          transport: "fetch-stream",
          protocol: "http1",
        },
        {
          baseUrl: "https://proxy.test",
          transport: "fetch-stream",
          protocol: "negotiated",
        },
        ...(withH2
          ? [
              {
                baseUrl: "https://meter.test:7248",
                transport: "fetch-stream",
                protocol: "http2" as const,
              },
            ]
          : []),
      ],
      latency: [{ baseUrl: "http://meter.test:7246", transport: "websocket" }],
    },
  });

  try {
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL("http://meter.test:7246/"),
    });
    globalThis.Worker = FakeWorker as unknown as typeof Worker;
    performance.getEntriesByName = () =>
      [{ nextHopProtocol: browserProtocol }] as unknown as PerformanceEntry[];
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      fetchUrls.push(url);
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

    const config: RunnerConfig = {
      stages: {
        latency: true,
        download: true,
        upload: true,
        bidirectional: false,
      },
      skipLoadedLatencyWhenStageOff: true,
      transports: {
        throughputTarget: "http://meter.test:7246",
        latencyTarget: "auto",
      },
      transferStreams: { mode: "forced", count: 6 },
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
        glideMs: 0,
      },
      visualization: { throughputMaxBytesPerSec: "auto" },
    };
    const failures: string[] = [];
    const discoveries: import("./contract").TransportDiscovery[] = [];
    const uploadBytes: number[] = [];
    const host = {
      config,
      phase: "idle",
      elapsed: 0,
      emit(event) {
        if (event.type === "transportDiscovery")
          discoveries.push(event.discovery);
      },
      reportTransport() {},
      fail() {},
      failStage(_stage, _reason, message) {
        failures.push(message);
      },
      ingestThroughput(_dir, _rate, bytes) {
        uploadBytes.push(bytes);
      },
      ingestLatency() {},
      stall() {},
      resume() {},
    } as CoreHost;
    const backend = new RealBackend();
    backend.attach(host);

    await backend.probe(config);
    config.transports.throughputTarget = "https://meter.test:7249";
    await expect(backend.probe(config)).rejects.toBeInstanceOf(
      TransportUnavailableError,
    );
    expect(
      discoveries.at(-1)?.throughput["https://meter.test:7248"].state,
    ).toBe("advertised");
    config.transports.throughputTarget = "http://meter.test:7246";
    const secondProbe = backend.probe(config);
    for (let i = 0; i < 10; i++) await Promise.resolve();
    pingWorker!.emit({
      type: "samples",
      samples: Array.from({ length: 5 }, () => ({ rtt: 1, lost: false })),
    });
    const info = await secondProbe;
    expect(pingMessages).toContainEqual({
      type: "measure",
      intervalMs: 1000,
    });
    expect(info.latencyClientIp).toBe("127.0.0.1");
    expect(info.latencyClientIpVersion).toBe(4);
    expect(info.latencyClientIpSource).toBe("socket");
    expect(info.latencyProtocolNegotiated).toBe("http/1.1");
    expect(preflights).toBe(3);
    const preflightUrls = fetchUrls.filter((url) => url.includes("/preflight"));
    expect(preflightUrls).toHaveLength(3);
    expect(
      preflightUrls.every((url) =>
        url.startsWith("/preflight?client=web&client_version="),
      ),
    ).toBe(true);
    expect(
      workerStarts
        .filter(({ kind }) => kind === "ping")
        .every(({ url }) => url === "ws://meter.test:7246/ws/ping"),
    ).toBe(true);
    expect(workerStarts.some(({ url }) => url.includes("8765"))).toBe(false);
    expect(
      discoveries[0].throughput["https://meter.test:7248"],
    ).toBeUndefined();
    expect(discoveries[2].throughput["https://meter.test:7248"].state).toBe(
      "advertised",
    );

    let fetchStart = fetchUrls.length;
    await backend.probe(config, undefined, "throughput");
    expect(fetchUrls.slice(fetchStart)).toHaveLength(2);

    fetchStart = fetchUrls.length;
    const latencyProbe = backend.probe(config, undefined, "latency");
    for (let i = 0; i < 10; i++) await Promise.resolve();
    pingWorker!.emit({
      type: "samples",
      samples: Array.from({ length: 5 }, () => ({ rtt: 1, lost: false })),
    });
    await latencyProbe;
    expect(fetchUrls.slice(fetchStart)).toHaveLength(2);

    config.transports.throughputTarget = "https://proxy.test";
    browserProtocol = "";
    const proxyWithoutTiming = await backend.probe(
      config,
      undefined,
      "throughput",
    );
    expect(proxyWithoutTiming.selectedThroughputProtocol).toBe("negotiated");
    expect(
      discoveries.at(-1)?.throughput["https://proxy.test"].target?.protocol,
    ).toBe("negotiated");

    browserProtocol = "h2";
    const proxyThroughput = await backend.probe(
      config,
      undefined,
      "throughput",
    );
    expect(proxyThroughput.selectedThroughputProtocol).toBe("http2");
    const proxyLatency = await backend.probe(config, undefined, "latency");
    expect(proxyLatency.selectedThroughputProtocol).toBe("http2");
    expect(
      discoveries.at(-1)?.throughput["https://proxy.test"].target?.protocol,
    ).toBe("negotiated");

    config.transports.throughputTarget = "http://meter.test:7246";
    browserProtocol = "http/1.1";
    await backend.probe(config, undefined, "throughput");

    backend.onRunStart(config);
    const unloaded: PhaseActivity = {
      stage: "latency",
      transfer: [],
      loadedLatency: false,
    };
    backend.onStageBegin(unloaded);
    expect(pingMessages.at(-1)).toMatchObject({
      type: "start",
      replyDriven: true,
      maxInFlight: 4,
    });
    backend.onStageMeasure(unloaded);
    expect(pingMessages.at(-1)).toEqual({ type: "measure" });
    await backend.onStageEnd(unloaded);

    const loaded: PhaseActivity = {
      stage: "download",
      transfer: ["down"],
      loadedLatency: true,
    };
    backend.onStageBegin(loaded);
    expect(pingMessages.at(-1)).toMatchObject({
      type: "start",
      intervalMs: 250,
      replyDriven: false,
      maxInFlight: 2,
    });
    backend.onStageMeasure(loaded);
    expect(pingMessages.at(-1)).toEqual({ type: "measure" });
    await backend.onStageEnd(loaded);
    started.length = 0;
    uploadBytes.length = 0;

    const preparation = backend.onStageBegin({
      stage: "upload",
      transfer: ["up"],
      loadedLatency: false,
    });
    for (let i = 0; i < 10 && !progressWorker; i++) await Promise.resolve();
    expect(started).toEqual(["progress"]);
    expect(workerStarts.at(-1)?.url).toBe(
      "http://meter.test:7246/upload/progress?id=gmu_test",
    );

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
    const activity: PhaseActivity = {
      stage: "upload",
      transfer: ["up"],
      loadedLatency: false,
    };
    backend.onStageMeasure(activity);
    progressWorker!.emit({ type: "bytes", n: 100, t: 100_000_000 });
    progressWorker!.emit({ type: "bytes", n: 200, t: 200_000_000 });
    const ending = backend.onStageEnd(activity);
    if (!ending)
      throw new Error("upload finalization did not return a promise");
    let ended = false;
    void ending.then(() => (ended = true));
    await Promise.resolve();
    expect(ended).toBe(false);

    progressWorker!.emit({ type: "complete", n: 300, t: 300_000_000 });
    await ending;
    expect(uploadBytes).toEqual([100, 100]);
    expect(ended).toBe(true);
    backend.onComplete();
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

// The transfer path dispatches on transport kind: a kind #transportOrder can
// emit but #primeTransfer does not recognise would fall through to the fetch
// branch and measure the wrong thing while reporting the selected transport.
// The classifier is the single source both sides read, and its `never` default
// makes an unclassified kind a compile error.
test("every throughput transport is classified as session-borne or fetch", () => {
  expect(throughputRidesSession("webtransport")).toBe(true);
  expect(throughputRidesSession("webtransport-datagram")).toBe(true);
  expect(throughputRidesSession("fetch-stream")).toBe(false);
  expect(throughputRidesSession("websocket")).toBe(false);
});
