import { test, expect } from "bun:test";
import type { CoreHost } from "./core";
import {
  httpToWs,
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
  targetOfKind,
  ROUTES,
} from "./real/backendPure";
import { TRANSPORTS, kindsForRole, ridesSession } from "./real/transports";
import type {
  InfraInfo,
  PhaseActivity,
  RunnerConfig,
  StallInfo,
  TransportDiscovery,
  TransportKind,
} from "./contract";
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
  expect(
    targetOfKind(catalog.throughput["https://meter.example"], "fetch-stream")
      ?.protocol,
  ).toBe("http1");
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

// An IPv6 literal carries "::" of its own, which is why a selection is matched
// whole and never taken apart to find the origin inside it.
test("an IPv6 origin resolves each of its mechanisms", () => {
  const origin = "https://[2001:db8::1]:7249";
  const catalog = classifyTransportDiscovery(
    [
      { baseUrl: origin, transport: "fetch-stream", protocol: "http3" },
      { baseUrl: origin, transport: "webtransport", protocol: "http3" },
    ],
    [{ baseUrl: origin, transport: "webtransport" }],
    origin,
    true,
    "h3",
  );
  expect(selectThroughputTarget(catalog, origin)?.transport).toBe(
    "fetch-stream",
  );
  expect(selectThroughputTarget(catalog, `${origin}::wt`)?.transport).toBe(
    "webtransport",
  );
  expect(selectLatencyTarget(catalog, `${origin}::wt`, true)?.transport).toBe(
    "webtransport",
  );
  // The origin has no WebSocket bus, so a plain selection resolves its only one.
  expect(selectLatencyTarget(catalog, origin, true)?.transport).toBe(
    "webtransport",
  );
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
  const wtStreams = targetOfKind(entry, "webtransport");
  const datagram = targetOfKind(entry, "webtransport-datagram");
  expect(targetOfKind(entry, "fetch-stream")?.transport).toBe("fetch-stream");
  expect(wtStreams?.id).toBe("https://meter:7249::wt");
  expect(
    wtStreams && "wtDownload" in wtStreams.routes
      ? wtStreams.routes.wtDownload
      : undefined,
  ).toBe(ROUTES.wtDownload);
  // The datagram path is its own advertised view, never folded onto the streams one.
  expect(datagram?.id).toBe("https://meter:7249::wtdg");
  expect(datagram?.transport).toBe("webtransport-datagram");

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
  expect(targetOfKind(entry, "websocket")?.transport).toBe("websocket");
  expect(targetOfKind(entry, "webtransport")?.transport).toBe("webtransport");
  expect(targetOfKind(entry, "webtransport")?.id).toBe("https://meter::wt");

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

test("a legacy latency target without a transport remains a WebSocket bus", () => {
  const catalog = classifyTransportDiscovery(
    [{ baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" }],
    [{ baseUrl: "." }],
    "https://meter.test",
    true,
  );

  expect(selectLatencyTarget(catalog, "auto")?.transport).toBe("websocket");
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
  const transferWorkers: FakeWorker[] = [];
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
      if (this.kind === "upload" || this.kind === "download")
        transferWorkers.push(this);
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

  /** Run a probe to completion, offering the keepalive RTTs it collects on
   *  every turn. The collection installs several awaits into probe() with
   *  nothing observable to wait on, so re-offering the samples pins the
   *  collected median instead of counting turns until it happens to be there.
   *  The fakes all resolve immediately, so turns are the only wait. */
  const probeWithRtts = async (
    probe: Promise<InfraInfo>,
    rtt: number,
  ): Promise<InfraInfo> => {
    let settled = false;
    const done = probe.then(
      (info) => {
        settled = true;
        return info;
      },
      (error: unknown) => {
        settled = true;
        throw error;
      },
    );
    for (let turn = 0; turn < 100 && !settled; turn++) {
      pingWorker?.emit({
        type: "samples",
        samples: Array.from({ length: 5 }, () => ({ rtt, lost: false })),
      });
      await Promise.resolve();
    }
    return done;
  };

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
        confirmationMs: 0,
      },
      visualization: { throughputMaxBytesPerSec: "auto" },
    };
    const failures: string[] = [];
    const discoveries: import("./contract").TransportDiscovery[] = [];
    const uploadBytes: number[] = [];
    const stalls: StallInfo[] = [];
    const host = {
      config,
      phase: "idle",
      elapsed: 0,
      emit(event) {
        if (event.type === "transportDiscovery")
          discoveries.push(event.discovery);
      },
      fail() {},
      failStage(_stage, _reason, message) {
        failures.push(message);
      },
      ingestThroughput(_dir, _rate, bytes) {
        uploadBytes.push(bytes);
      },
      ingestLatency() {},
      recordRecoveryGap() {},
      stall(info) {
        stalls.push(info);
      },
      resume() {},
    } as CoreHost;
    const backend = new RealBackend();
    backend.attach(host);

    const firstProbe = await probeWithRtts(backend.probe(config), 3);
    expect(firstProbe.preTestPingMs).toBe(3);
    config.transports.throughputTarget = "https://meter.test:7249";
    await expect(backend.probe(config)).rejects.toBeInstanceOf(
      TransportUnavailableError,
    );
    expect(
      discoveries.at(-1)?.throughput["https://meter.test:7248"].state,
    ).toBe("advertised");
    config.transports.throughputTarget = "http://meter.test:7246";
    const info = await probeWithRtts(backend.probe(config), 5);
    expect(pingMessages).toContainEqual({
      type: "measure",
      intervalMs: 1000,
    });
    // The collected RTTs are the pre-test ping median, so a collection that
    // ran late and resolved empty would report the previous probe's value.
    expect(info.preTestPingMs).toBe(5);
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
    const latencyProbe = await probeWithRtts(
      backend.probe(config, undefined, "latency"),
      7,
    );
    expect(latencyProbe.preTestPingMs).toBe(7);
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
      targetOfKind(
        discoveries.at(-1)!.throughput["https://proxy.test"],
        "fetch-stream",
      )?.protocol,
    ).toBe("negotiated");

    browserProtocol = "h2";
    const proxyThroughput = await backend.probe(
      config,
      undefined,
      "throughput",
    );
    expect(proxyThroughput.selectedThroughputProtocol).toBe("http2");
    const proxyLatency = await probeWithRtts(
      backend.probe(config, undefined, "latency"),
      9,
    );
    expect(proxyLatency.selectedThroughputProtocol).toBe("http2");
    expect(proxyLatency.preTestPingMs).toBe(9);
    expect(
      targetOfKind(
        discoveries.at(-1)!.throughput["https://proxy.test"],
        "fetch-stream",
      )?.protocol,
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
    transferWorkers.at(-1)!.emit({
      type: "error",
      recoverable: true,
      detail: "download stalled",
    });
    expect(stalls.at(-1)?.transport).toBe("fetch-stream");
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

// ---------------------------------------------------------------------------
// Probe lifecycle: supersession and the hidden-page keepalive. Both drive a real
// RealBackend, so each needs the build tokens buildenv reads at import time plus
// the DOM seams a probe touches.
// ---------------------------------------------------------------------------

const BUILD_TOKENS = {
  __GM_DEFAULT_ENGINE__: "real",
  __GM_ALLOW_DUMMY__: false,
  __GM_DEV_TOOLS__: false,
  __GM_BUILD_LABEL__: "test",
  __GM_CLIENT_VERSION__: "0.0.0-test",
};

const preflightDocument = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [
      {
        baseUrl: "http://meter.test:7246",
        transport: "fetch-stream",
        protocol: "http1",
      },
    ],
    latency: [{ baseUrl: "http://meter.test:7246", transport: "websocket" }],
  },
};

const pathProbeDocument = {
  clientIp: "127.0.0.1",
  clientIpVersion: 4,
  clientIpSource: "socket",
  protocolNegotiated: "http/1.1",
};

/** Install the globals a probe reads; the returned callback puts them back. */
function stubProbeEnvironment(fetchImpl: typeof fetch): () => void {
  const buildGlobals = globalThis as typeof globalThis &
    Record<string, unknown>;
  Object.assign(buildGlobals, BUILD_TOKENS);
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: new URL("http://meter.test:7246/"),
  });
  performance.getEntriesByName = () =>
    [{ nextHopProtocol: "http/1.1" }] as unknown as PerformanceEntry[];
  globalThis.fetch = fetchImpl;
  return () => {
    globalThis.fetch = realFetch;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
    else Reflect.deleteProperty(globalThis, "location");
    performance.getEntriesByName = realEntries;
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(buildGlobals, key);
  };
}

const probeConfig = (latency: boolean): RunnerConfig => ({
  stages: { latency, download: true, upload: false, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  transports: {
    throughputTarget: "http://meter.test:7246",
    latencyTarget: "auto",
  },
  transferStreams: { mode: "auto", count: 1 },
  duration: {
    warmupMs: 0,
    latencyMs: 1,
    downloadMs: 1,
    uploadMs: 0,
    bidirectionalMs: 0,
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
});

// `selectThroughputTarget`'s `webTransport` parameter defaults on where
// `selectLatencyTarget`'s defaults off, and #selectThroughputRole is the reason:
// it wants the raw advertisement so it can resolve first and refuse second,
// naming the mechanism. Feeding it the browser's real capability instead — the
// symmetry a reader is tempted by — returns null for a WebTransport-only origin
// and degrades the refusal to "auto target unavailable", which blames the
// server for a client limitation. Both halves are asserted so the trade is
// visible: what the runner is handed, and what the tempting change would hand it.
test("a WebTransport-less browser is refused by mechanism, not by availability", async () => {
  const catalog = classifyTransportDiscovery(
    [
      {
        baseUrl: "https://wt.meter.test",
        transport: "webtransport",
        protocol: "http3",
      },
    ],
    [],
    "http://meter.test:7246",
    false,
    "http/1.1",
  );
  // What #selectThroughputRole is handed today, and with an explicit `true`.
  expect(selectThroughputTarget(catalog, "auto")?.transport).toBe(
    "webtransport",
  );
  // What passing `transportRunnable("webtransport")` would hand it in a browser
  // without the API: nothing to name, so the throw upstream of the refusal wins.
  expect(selectThroughputTarget(catalog, "auto", false)).toBeNull();

  const globals = globalThis as typeof globalThis & Record<string, unknown>;
  const realWebTransport = Object.getOwnPropertyDescriptor(
    globalThis,
    "WebTransport",
  );
  Reflect.deleteProperty(globals, "WebTransport");
  const restore = stubProbeEnvironment((async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes("/preflight"))
      return Response.json({
        server: { name: "test" },
        engineVersion: "test",
        generation: "a",
        capabilities: {
          throughput: [
            {
              baseUrl: "https://wt.meter.test",
              transport: "webtransport",
              protocol: "http3",
            },
          ],
          latency: [],
        },
      });
    if (url.includes("/probe")) return Response.json(pathProbeDocument);
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch);
  try {
    const { RealBackend } = await import("./RealRunner");
    const backend = new RealBackend();
    backend.attach({ emit() {} } as unknown as CoreHost);
    await expect(
      backend.probe({
        ...probeConfig(false),
        transports: { throughputTarget: "auto", latencyTarget: "auto" },
      }),
    ).rejects.toThrow(/^webtransport is not supported by this client$/);
  } finally {
    restore();
    if (realWebTransport)
      Object.defineProperty(globalThis, "WebTransport", realWebTransport);
  }
});

// engine.svelte.ts reads a discovery generation change as a server swap: it
// drops the prepared selection and marks both roles stale. A superseded probe
// emitting on its way out would re-open the validation loop the newer probe
// just closed, so the epoch guard has to cover the emit, not only what follows
// it.
test("a superseded probe does not publish its discovery", async () => {
  let releaseFirst = (): void => {};
  let preflights = 0;
  const restore = stubProbeEnvironment((async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes("/preflight")) {
      preflights++;
      // Hold the first probe inside its discovery fetch, so a newer one takes
      // over before it resumes.
      if (preflights === 1)
        await new Promise<void>((resolve) => (releaseFirst = resolve));
      return Response.json(preflightDocument);
    }
    if (url.includes("/probe")) return Response.json(pathProbeDocument);
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch);
  try {
    const { RealBackend } = await import("./RealRunner");
    const discoveries: TransportDiscovery[] = [];
    const backend = new RealBackend();
    backend.attach({
      emit(event) {
        if (event.type === "transportDiscovery")
          discoveries.push(event.discovery);
      },
    } as CoreHost);

    const superseded = backend.probe(probeConfig(false));
    for (let turn = 0; turn < 20 && preflights < 1; turn++)
      await Promise.resolve();
    await backend.probe(probeConfig(false));
    expect(discoveries).toHaveLength(1);

    releaseFirst();
    const outcome = await superseded.then(
      () => "resolved",
      (cause: unknown) => (cause as Error).message,
    );
    expect(outcome).toBe("probe superseded");
    expect(discoveries).toHaveLength(1);
  } finally {
    restore();
  }
});

// Chromium throttles a hidden page's dedicated workers to roughly one timer
// wake a minute after five minutes hidden, far outside the ~30 s the server
// gives a ping bus with nothing arriving on it. A keepalive left running in a
// hidden tab is therefore reaped, reconnected and reaped again, latching the
// pill offline each time. Parking on visibilitychange alone does not cover it:
// a probe starts the keepalive, and an `online` edge or a boot in a background
// tab probes while hidden.
test("a hidden page parks the keepalive its probe started, and gets it back on visibility", async () => {
  class FakePingWorker {
    static all: FakePingWorker[] = [];
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: ((event: ErrorEvent) => void) | null = null;
    terminated = false;

    constructor() {
      FakePingWorker.all.push(this);
    }
    postMessage(): void {}
    terminate(): void {
      this.terminated = true;
    }
    emit(data: unknown): void {
      this.onmessage?.({ data } as MessageEvent);
    }
  }

  const restore = stubProbeEnvironment((async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes("/preflight")) return Response.json(preflightDocument);
    if (url.includes("/probe")) return Response.json(pathProbeDocument);
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch);
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakePingWorker as unknown as typeof Worker;
  try {
    const { RealBackend } = await import("./RealRunner");
    const connectivity: string[] = [];
    const backend = new RealBackend();
    backend.attach({
      emit(event) {
        if (event.type === "connectivity") connectivity.push(event.state);
      },
    } as CoreHost);

    backend.setBackgroundActivity(false); // the page is hidden
    // The keepalive is the probe's readiness and RTT source. Its collection
    // reuses a worker started before the callback is installed, so the samples
    // are re-offered every turn rather than counted.
    let settled = false;
    const probe = backend.probe(probeConfig(true)).finally(() => {
      settled = true;
    });
    for (let turn = 0; turn < 100 && !settled; turn++) {
      FakePingWorker.all.at(-1)?.emit({ type: "ready" });
      FakePingWorker.all.at(-1)?.emit({
        type: "samples",
        samples: Array.from({ length: 5 }, () => ({ rtt: 3, lost: false })),
      });
      await Promise.resolve();
    }
    await probe;

    const parked = FakePingWorker.all.at(-1)!;
    expect(parked.terminated).toBe(true);
    // Nothing is watching a hidden page, and its bus dying is expected there.
    const emitted = connectivity.length;
    parked.emit({ type: "stall", detail: "webtransport closed" });
    expect(connectivity).toHaveLength(emitted);

    const workers = FakePingWorker.all.length;
    backend.setBackgroundActivity(true);
    expect(FakePingWorker.all).toHaveLength(workers + 1);
  } finally {
    globalThis.Worker = realWorker;
    restore();
  }
});

/** One origin advertising both ping buses next to a fetch throughput target:
 *  the shape a proxy serving TCP and UDP on one hostname takes. */
const bothBusesDocument = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [
      {
        baseUrl: "https://meter.test",
        transport: "fetch-stream",
        protocol: "http2",
      },
    ],
    latency: [
      { baseUrl: "https://meter.test", transport: "websocket" },
      { baseUrl: "https://meter.test", transport: "webtransport" },
    ],
  },
};

// The latency channel check degrades a WebTransport ping bus that never
// establishes to the origin's WebSocket bus. A throughput-role probe does not
// run that check — it carries the latency role's evidence over — so it must not
// re-run the selector either: re-selecting rebound the run to the bus the check
// had just proved dead, and the latency stage then sat out its establish budget
// and was skipped.
test("a throughput-role probe keeps the latency bus the last check committed to", async () => {
  const pingStarts: string[] = [];
  class PingBusWorker {
    static live: PingBusWorker[] = [];
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: ((event: ErrorEvent) => void) | null = null;
    transport = "";

    constructor() {
      PingBusWorker.live.push(this);
    }

    postMessage(message: { type: string; transport?: string }): void {
      if (message.type !== "start") return;
      this.transport = message.transport ?? "";
      pingStarts.push(this.transport);
      // The WebTransport bus never answers, the shape of a path without UDP.
      if (this.transport === "websocket")
        queueMicrotask(() => this.emit({ type: "ready" }));
    }

    terminate(): void {}

    emit(data: unknown): void {
      this.onmessage?.({ data } as MessageEvent);
    }
  }

  const buildGlobals = globalThis as typeof globalThis &
    Record<string, unknown>;
  Object.assign(buildGlobals, BUILD_TOKENS);
  const realFetch = globalThis.fetch;
  const realWorker = globalThis.Worker;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  try {
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL("https://meter.test/"),
    });
    performance.getEntriesByName = () =>
      [{ nextHopProtocol: "h2" }] as unknown as PerformanceEntry[];
    globalThis.Worker = PingBusWorker as unknown as typeof Worker;
    // Never dialled: it is what makes the advertised WebTransport bus
    // selectable at all.
    globals.WebTransport = class {};
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(bothBusesDocument);
      if (url.includes("/probe")) return Response.json(pathProbeDocument);
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;

    const { RealBackend } = await import("./RealRunner");
    const config = probeConfig(true);
    config.stages.download = false;
    config.transports.throughputTarget = "https://meter.test";
    const backend = new RealBackend();
    backend.attach({
      config,
      emit() {},
      failStage() {},
      ingestLatency() {},
    } as unknown as CoreHost);

    // Real time has to pass here: the readiness budget the WebTransport bus
    // blows through is a timer, and the keepalive's RTT collection has nothing
    // observable to wait on, so its samples are re-offered every turn.
    let settled = false;
    const degrading = backend.probe(config).then(
      (info) => {
        settled = true;
        return info;
      },
      (error: unknown) => {
        settled = true;
        throw error;
      },
    );
    for (let turn = 0; turn < 1000 && !settled; turn++) {
      const bus = PingBusWorker.live.at(-1);
      if (bus?.transport === "websocket")
        bus.emit({
          type: "samples",
          samples: Array.from({ length: 5 }, () => ({ rtt: 2, lost: false })),
        });
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    expect((await degrading).selectedLatencyTransport).toBe("websocket");

    const throughputRole = await backend.probe(config, undefined, "throughput");
    expect(throughputRole.selectedLatencyTransport).toBe("websocket");

    // What the run actually primes, which is the sample source the latency
    // stage lives or dies by.
    backend.onRunStart(config);
    backend.onStageBegin({
      stage: "latency",
      transfer: [],
      loadedLatency: false,
    });
    expect(pingStarts.at(-1)).toBe("websocket");
    backend.dispose();
  } finally {
    globalThis.fetch = realFetch;
    globalThis.Worker = realWorker;
    performance.getEntriesByName = realEntries;
    if (realWebTransport === undefined)
      Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
    else Reflect.deleteProperty(globalThis, "location");
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(buildGlobals, key);
  }
}, 15000);

// The transfer path dispatches on what the registry says, so a kind missing a
// row would fall through to fetch and measure the wrong thing. Record<
// TransportKind, TransportSpec> makes that a compile error; this pins the
// values a row has to get right.
test("every transport has a registry row saying how it is driven", () => {
  const kinds: TransportKind[] = [
    "fetch-stream",
    "websocket",
    "webtransport",
    "webtransport-datagram",
  ];
  for (const kind of kinds) expect(TRANSPORTS[kind].kind).toBe(kind);
  expect(ridesSession("webtransport")).toBe(true);
  expect(ridesSession("webtransport-datagram")).toBe(true);
  expect(ridesSession("fetch-stream")).toBe(false);
  expect(ridesSession("websocket")).toBe(false);
  // Picker order per role, which is what an origin's cards are listed in.
  expect(kindsForRole("throughput")).toEqual([
    "fetch-stream",
    "webtransport",
    "webtransport-datagram",
  ]);
  expect(kindsForRole("latency")).toEqual(["websocket", "webtransport"]);
});

// The datagram setting lists a card. Selection, dispatch and the registry are
// blind to it, so a saved selection keeps working with the setting off.
test("the datagram setting does not reach selection or dispatch", () => {
  const origin = "https://meter:7249";
  const catalog = classifyTransportDiscovery(
    [
      { baseUrl: origin, transport: "fetch-stream", protocol: "http3" },
      {
        baseUrl: origin,
        transport: "webtransport-datagram",
        protocol: "http3",
      },
    ],
    [],
    origin,
    true,
    "h3",
  );
  const datagram = selectThroughputTarget(catalog, `${origin}::wtdg`);
  expect(datagram?.transport).toBe("webtransport-datagram");
  expect(ridesSession(datagram!.transport)).toBe(true);
  expect(kindsForRole("throughput")).toContain("webtransport-datagram");
});
