import { test, expect } from "bun:test";
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
import { DEFAULT_CONFIG } from "../state/defaults";
import { TEST_BUILD_TOKENS, testHost, testTransfer } from "./test-helpers.test";
type ThroughputAdvertisement = Parameters<
  typeof classifyTransportDiscovery
>[0][number];
type LatencyAdvertisement = Parameters<
  typeof classifyTransportDiscovery
>[1][number];
const discovery = (
  throughput: ThroughputAdvertisement[],
  latency: LatencyAdvertisement[] = [],
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
const phaseActivity = (
  stage: PhaseActivity["stage"],
  transfer: PhaseActivity["transfer"] = [],
  loadedLatency = false,
): PhaseActivity => ({ stage, transfer, loadedLatency });
const fetchAd = (
  baseUrl: string,
  protocol: ThroughputAdvertisement["protocol"] = "http3",
): ThroughputAdvertisement => ({
  baseUrl,
  transport: "fetch-stream",
  protocol,
});
const wtAd = (baseUrl: string): ThroughputAdvertisement => ({
  baseUrl,
  transport: "webtransport",
  protocol: "http3",
});
const dgAd = (baseUrl: string): ThroughputAdvertisement => ({
  baseUrl,
  transport: "webtransport-datagram",
  protocol: "http3",
});
const wsAd = (baseUrl: string): LatencyAdvertisement => ({
  baseUrl,
  transport: "websocket",
});
const wtLatencyAd = (baseUrl: string): LatencyAdvertisement => ({
  baseUrl,
  transport: "webtransport",
});
const pingSamples = (rtt: number) =>
  Array.from({ length: 5 }, () => ({ rtt, lost: false }));

test("proxy endpoints resolve relative to preflight and negotiate the browser hop", () => {
  const catalog = discovery(
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
  const catalog = discovery(
    [fetchAd("https://meter.example", "http1"), fetchAd(".", "negotiated")],
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
  const catalog = discovery(
    [
      fetchAd("http://meter:7246", "http1"),
      fetchAd("https://meter:7248", "http2"),
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

test("an IPv6 origin resolves each of its mechanisms", () => {
  const origin = "https://[2001:db8::1]:7249";
  const catalog = discovery(
    [fetchAd(origin), wtAd(origin)],
    [wtLatencyAd(origin)],
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
  expect(selectLatencyTarget(catalog, origin, true)?.transport).toBe(
    "webtransport",
  );
});

test("WebTransport folds onto its origin and leads latency auto-selection", () => {
  const catalog = discovery(
    [
      fetchAd("https://meter:7249"),
      wtAd("https://meter:7249"),
      dgAd("https://meter:7249"),
    ],
    [wsAd("https://meter:7247"), wtLatencyAd("https://meter:7249")],
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
  expect(datagram?.id).toBe("https://meter:7249::wtdg");
  expect(datagram?.transport).toBe("webtransport-datagram");
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
  expect(selectLatencyTarget(catalog, "auto")?.transport).toBe("websocket");
  expect(selectLatencyTarget(catalog, "https://meter:7249", false)).toBeNull();
  const wt = selectLatencyTarget(catalog, "auto", true);
  expect(wt?.transport).toBe("webtransport");
  expect(wt?.origin).toBe("https://meter:7249");
});

test("one origin advertising both latency buses keeps the WebSocket fallback", () => {
  const catalog = discovery(
    [fetchAd("https://meter")],
    [wsAd("https://meter"), wtLatencyAd("https://meter")],
    "https://meter",
    true,
    "h3",
  );
  const entry = catalog.latency["https://meter"];
  expect(targetOfKind(entry, "websocket")?.transport).toBe("websocket");
  expect(targetOfKind(entry, "webtransport")?.transport).toBe("webtransport");
  expect(targetOfKind(entry, "webtransport")?.id).toBe("https://meter::wt");
  expect(selectLatencyTarget(catalog, "auto", true)?.transport).toBe(
    "webtransport",
  );
  expect(selectLatencyTarget(catalog, "auto", false)?.transport).toBe(
    "websocket",
  );
  expect(
    selectLatencyTarget(catalog, "https://meter::wt", true)?.transport,
  ).toBe("webtransport");
  expect(selectLatencyTarget(catalog, "https://meter", false)?.transport).toBe(
    "websocket",
  );
});

test("a WebTransport-only origin is auto's last resort and keeps a fetch view", () => {
  const catalog = discovery(
    [wtAd("https://meter:7249")],
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
  const catalog = discovery(
    [fetchAd(".", "negotiated")],
    [{ baseUrl: "." }],
    "https://meter.test",
    true,
  );
  expect(selectLatencyTarget(catalog, "auto")?.transport).toBe("websocket");
});

test("browser protocol verification is independent of server probe evidence", () => {
  const h1 = testTransfer("http1-tls", "https://meter", "http1", true);
  const h2 = testTransfer("http2", "https://meter", "http2", true);
  const negotiated = testTransfer(
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
  const target = testTransfer("http2", "https://meter", "http2", true);
  expect(throughputTargetKey(target)).toBe("http2\nhttps://meter");
  expect(
    throughputTargetKey({ ...target, origin: "https://other-meter" }),
  ).not.toBe(throughputTargetKey(target));
});

test("clear loopback targets stay usable from HTTPS", () => {
  for (const host of ["localhost", "meter.localhost", "127.42.0.9", "[::1]"]) {
    const target = testTransfer(
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

test("httpToWs: maps https:// to wss:// and http:// to ws://", () => {
  expect(httpToWs("https://example.com:443")).toBe("wss://example.com:443");
  expect(httpToWs("http://example.com:7246")).toBe("ws://example.com:7246");
});

test("httpToWs: passes through anything already ws(s):// or relative", () => {
  expect(httpToWs("wss://example.com")).toBe("wss://example.com");
  expect(httpToWs("ws://example.com")).toBe("ws://example.com");
  expect(httpToWs("")).toBe("");
});
const activity = (overrides: Partial<PhaseActivity> = {}): PhaseActivity => ({
  stage: "download",
  transfer: ["down"],
  loadedLatency: false,
  ...overrides,
});
for (const { name, input, expected } of [
  {
    name: "the latency stage always needs pings",
    input: activity({ stage: "latency", transfer: [], loadedLatency: false }),
    expected: true,
  },
  {
    name: "a transfer stage needs pings when loadedLatency is on",
    input: activity({ loadedLatency: true }),
    expected: true,
  },
  {
    name: "a transfer stage does not need pings when loadedLatency is off",
    input: activity({ loadedLatency: false }),
    expected: false,
  },
  {
    name: "loadedLatency alone is not enough without transfer lanes",
    input: activity({ transfer: [], loadedLatency: true, stage: "download" }),
    expected: false,
  },
] satisfies Array<{ name: string; input: PhaseActivity; expected: boolean }>)
  test(`needsPings: ${name}`, () => {
    expect(needsPings(input)).toBe(expected);
  });
for (const { name, lanes, warmupMs, baseMs, expected } of [
  {
    name: "a single lane never staggers",
    lanes: 1,
    warmupMs: 4000,
    baseMs: 75,
    expected: 0,
  },
  {
    name: "zero lanes never staggers",
    lanes: 0,
    warmupMs: 4000,
    baseMs: 75,
    expected: 0,
  },
  {
    name: "zero warmup spawns lanes together",
    lanes: 4,
    warmupMs: 0,
    baseMs: 75,
    expected: 0,
  },
  {
    name: "splits half the warmup across non-first lanes",
    lanes: 4,
    warmupMs: 3000,
    baseMs: 500,
    expected: 500,
  },
  {
    name: "caps at the base stagger on a long warmup",
    lanes: 2,
    warmupMs: 100_000,
    baseMs: 75,
    expected: 75,
  },
] satisfies Array<{
  name: string;
  lanes: number;
  warmupMs: number;
  baseMs: number;
  expected: number;
}>)
  test(`laneStaggerMs: ${name}`, () => {
    expect(laneStaggerMs(lanes, warmupMs, baseMs)).toBe(expected);
  });

test("real backend: probe refresh keeps the negotiated protocol per role, and the upload stage opens its progress channel before any POST lane", async () => {
  const realWorker = globalThis.Worker;
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
      if (this.kind === "ping" && message.type === "stop") {
        queueMicrotask(() => this.emit({ type: "stopped" }));
        return;
      }
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
        samples: pingSamples(rtt),
      });
      await Promise.resolve();
    }
    return done;
  };
  const restoreProbe = stubProbeEnvironment(
    (async (input: RequestInfo | URL) => {
      const url = String(input);
      fetchUrls.push(url);
      if (url.includes("/preflight")) {
        preflights++;
        return Response.json(probeDiscovery(preflights > 1));
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
    }) as typeof fetch,
    { location: "http://meter.test:7246/", protocol: () => browserProtocol },
  );
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
    const { RealBackend, TransportUnavailableError } =
      await import("./RealRunner");
    const config = probeConfig(true);
    config.stages = {
      latency: true,
      download: true,
      upload: true,
      bidirectional: false,
    };
    config.transports.throughputTarget = "http://meter.test:7246";
    config.transferStreams = { mode: "forced", count: 6 };
    config.duration = {
      warmupMs: 0,
      latencyMs: 1,
      downloadMs: 1,
      uploadMs: 1,
      bidirectionalMs: 1,
    };
    const failures: string[] = [];
    let incompleteAccounting = 0;
    const discoveries: import("./contract").TransportDiscovery[] = [];
    const uploadBytes: number[] = [];
    const stalls: StallInfo[] = [];
    const host = testHost(config, {
      emit(event) {
        if (event.type === "transportDiscovery")
          discoveries.push(event.discovery);
      },
      failStage(_stage, _reason, message) {
        failures.push(message);
      },
      ingestLatencyAccountingIncomplete() {
        incompleteAccounting++;
      },
      ingestThroughput(_dir, _rate, bytes) {
        uploadBytes.push(bytes);
      },
      stall(info) {
        stalls.push(info);
      },
    });
    const backend = new RealBackend();
    backend.attach(host);
    const probe = async () => {
      try {
        const info = await backend.probe(config);
        discoveries.push(info.discovery!);
        return info;
      } catch (cause) {
        if (cause instanceof TransportUnavailableError && cause.discovery)
          discoveries.push(cause.discovery);
        throw cause;
      }
    };
    const firstProbe = await probeWithRtts(probe(), 3);
    expect(firstProbe.preTestPingMs).toBe(3);
    config.transports.throughputTarget = "https://meter.test:7249";
    await expect(probe()).rejects.toBeInstanceOf(TransportUnavailableError);
    expect(
      discoveries.at(-1)?.throughput["https://meter.test:7248"].state,
    ).toBe("advertised");
    config.transports.throughputTarget = "http://meter.test:7246";
    const info = await probeWithRtts(probe(), 5);
    expect(pingMessages).toContainEqual({
      type: "measure",
      intervalMs: 1000,
    });
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
    const unloaded = phaseActivity("latency");
    backend.onStageBegin(unloaded);
    expect(pingMessages.at(-1)).toMatchObject({
      type: "start",
      replyDriven: true,
      maxInFlight: 4,
    });
    backend.onStageMeasure(unloaded);
    expect(pingMessages.at(-1)).toEqual({ type: "measure" });
    await backend.onStageEnd(unloaded);
    const loaded = phaseActivity("download", ["down"], true);
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
    expect(incompleteAccounting).toBe(0);
    backend.onStageBegin(loaded);
    backend.onStageMeasure(loaded);
    backend.onStageEnd(loaded, false);
    expect(incompleteAccounting).toBe(1);
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
    const activity = phaseActivity("upload", ["up"]);
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
    uploadBytes.length = 0;
    progressWorker = null;
    const stalePreparation = backend.onStageBegin(activity);
    for (let i = 0; i < 10 && !progressWorker; i++) await Promise.resolve();
    progressWorker!.emit({ type: "open" });
    await stalePreparation;
    backend.onStageMeasure(activity);
    const staleEnding = backend.onStageEnd(activity);
    if (!staleEnding)
      throw new Error("stale upload finalization did not return a promise");
    backend.onAbort();
    backend.onRunStart(config);
    progressWorker = null;
    const replacementPreparation = backend.onStageBegin(activity);
    for (let i = 0; i < 10 && !progressWorker; i++) await Promise.resolve();
    progressWorker!.emit({ type: "open" });
    await replacementPreparation;
    backend.onStageMeasure(activity);
    await staleEnding;
    progressWorker!.emit({ type: "bytes", n: 100, t: 100_000_000 });
    progressWorker!.emit({ type: "bytes", n: 200, t: 200_000_000 });
    expect(uploadBytes).toEqual([100]);
    backend.onComplete();
  } finally {
    globalThis.Worker = realWorker;
    restoreProbe();
  }
});
const preflightDocument = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [fetchAd("http://meter.test:7246", "http1")],
    latency: [wsAd("http://meter.test:7246")],
  },
};
const probeDiscovery = (withH2: boolean) => ({
  ...preflightDocument,
  generation: withH2 ? "b" : "a",
  capabilities: {
    throughput: [
      fetchAd("http://meter.test:7246", "http1"),
      fetchAd("https://proxy.test", "negotiated"),
      ...(withH2 ? [fetchAd("https://meter.test:7248", "http2")] : []),
    ],
    latency: [wsAd("http://meter.test:7246")],
  },
});
const pathProbeDocument = {
  clientIp: "127.0.0.1",
  clientIpVersion: 4,
  clientIpSource: "socket",
  protocolNegotiated: "http/1.1",
};
function probeFetch(preflight = preflightDocument): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes("/preflight")) return Response.json(preflight);
    if (url.includes("/probe")) return Response.json(pathProbeDocument);
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
}
function stubProbeEnvironment(
  fetchImpl: typeof fetch,
  options: { location?: string; protocol?: string | (() => string) } = {},
): () => void {
  const buildGlobals = globalThis as typeof globalThis &
    Record<string, unknown>;
  Object.assign(buildGlobals, TEST_BUILD_TOKENS);
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: new URL(options.location ?? "http://meter.test:7246/"),
  });
  performance.getEntriesByName = () =>
    [
      {
        nextHopProtocol:
          typeof options.protocol === "function"
            ? options.protocol()
            : (options.protocol ?? "http/1.1"),
      },
    ] as unknown as PerformanceEntry[];
  globalThis.fetch = fetchImpl;
  return () => {
    globalThis.fetch = realFetch;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
    else Reflect.deleteProperty(globalThis, "location");
    performance.getEntriesByName = realEntries;
    for (const key of Object.keys(TEST_BUILD_TOKENS))
      Reflect.deleteProperty(buildGlobals, key);
  };
}
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
class PingBusWorker {
  static live: PingBusWorker[] = [];
  static starts: string[] = [];
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  transport = "";
  constructor() {
    PingBusWorker.live.push(this);
  }
  postMessage(message: { type: string; transport?: string }): void {
    if (message.type !== "start") return;
    this.transport = message.transport ?? "";
    PingBusWorker.starts.push(this.transport);
    if (this.transport === "websocket")
      queueMicrotask(() => this.emit({ type: "ready" }));
  }
  terminate(): void {}
  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}
const probeConfig = (latency: boolean): RunnerConfig => ({
  ...structuredClone(DEFAULT_CONFIG),
  stages: { latency, download: true, upload: false, bidirectional: false },
  transferStreams: { mode: "auto", count: 1 },
  transports: {
    throughputTarget: "http://meter.test:7246",
    latencyTarget: "auto",
  },
  duration: {
    warmupMs: 0,
    latencyMs: 1,
    downloadMs: 1,
    uploadMs: 0,
    bidirectionalMs: 0,
  },
  adaptive: {
    ...DEFAULT_CONFIG.adaptive,
    enabled: false,
    minCoverageRatio: 1,
    stabilityThreshold: 1,
    maxPhaseReductionRatio: 0,
    minLatencySamples: 1,
    minTransferSamples: 1,
    confirmationMs: 0,
  },
});
test("a WebTransport-less browser is refused by mechanism, not by availability", async () => {
  const catalog = discovery(
    [wtAd("https://wt.meter.test")],
    [],
    "http://meter.test:7246",
    false,
    "http/1.1",
  );
  expect(selectThroughputTarget(catalog, "auto")?.transport).toBe(
    "webtransport",
  );
  expect(selectThroughputTarget(catalog, "auto", false)).toBeNull();
  const globals = globalThis as typeof globalThis & Record<string, unknown>;
  const realWebTransport = Object.getOwnPropertyDescriptor(
    globalThis,
    "WebTransport",
  );
  Reflect.deleteProperty(globals, "WebTransport");
  const restore = stubProbeEnvironment(
    probeFetch({
      ...preflightDocument,
      capabilities: {
        throughput: [wtAd("https://wt.meter.test")],
        latency: [],
      },
    }),
  );
  try {
    const { RealBackend } = await import("./RealRunner");
    const backend = new RealBackend();
    const config = {
      ...probeConfig(false),
      transports: { throughputTarget: "auto", latencyTarget: "auto" },
    };
    backend.attach(testHost(config));
    await expect(backend.probe(config)).rejects.toThrow(
      /^webtransport is not supported by this client$/,
    );
  } finally {
    restore();
    if (realWebTransport)
      Object.defineProperty(globalThis, "WebTransport", realWebTransport);
  }
});

test("a superseded probe does not publish its discovery", async () => {
  let releaseFirst = (): void => {};
  let preflights = 0;
  const restore = stubProbeEnvironment((async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes("/preflight")) {
      preflights++;
      if (preflights === 1)
        await new Promise<void>((resolve) => (releaseFirst = resolve));
      return Response.json(preflightDocument);
    }
    return probeFetch()(input);
  }) as typeof fetch);
  try {
    const { RealBackend } = await import("./RealRunner");
    const discoveries: TransportDiscovery[] = [];
    const backend = new RealBackend();
    backend.attach(
      testHost(probeConfig(false), {
        emit(event) {
          if (event.type === "transportDiscovery")
            discoveries.push(event.discovery);
        },
      }),
    );
    const superseded = backend.probe(probeConfig(false));
    for (let turn = 0; turn < 20 && preflights < 1; turn++)
      await Promise.resolve();
    const prepared = await backend.probe(probeConfig(false));
    expect(prepared.discovery?.generation).toBe(preflightDocument.generation);
    expect(discoveries).toHaveLength(0);
    releaseFirst();
    const outcome = await superseded.then(
      () => "resolved",
      (cause: unknown) => (cause as Error).message,
    );
    expect(outcome).toBe("probe superseded");
    expect(discoveries).toHaveLength(0);
  } finally {
    restore();
  }
});

test("a hidden page parks the keepalive its probe started, and gets it back on visibility", async () => {
  FakePingWorker.all = [];
  const restore = stubProbeEnvironment(probeFetch());
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakePingWorker as unknown as typeof Worker;
  try {
    const { RealBackend } = await import("./RealRunner");
    const connectivity: string[] = [];
    const backend = new RealBackend();
    backend.attach(
      testHost(probeConfig(true), {
        emit(event) {
          if (event.type === "connectivity") connectivity.push(event.state);
        },
      }),
    );
    backend.setBackgroundActivity(false); // the page is hidden; the keepalive supplies probe readiness and RTT.
    let settled = false;
    const probe = backend.probe(probeConfig(true)).finally(() => {
      settled = true;
    });
    for (let turn = 0; turn < 100 && !settled; turn++) {
      FakePingWorker.all.at(-1)?.emit({ type: "ready" });
      FakePingWorker.all.at(-1)?.emit({
        type: "samples",
        samples: pingSamples(3),
      });
      await Promise.resolve();
    }
    await probe;
    const parked = FakePingWorker.all.at(-1)!;
    expect(parked.terminated).toBe(true);
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
const bothBusesDocument = {
  ...preflightDocument,
  capabilities: {
    throughput: [fetchAd("https://meter.test", "http2")],
    latency: [wsAd("https://meter.test"), wtLatencyAd("https://meter.test")],
  },
};

test("a throughput-role probe keeps the latency bus the last check committed to", async () => {
  PingBusWorker.live = [];
  PingBusWorker.starts = [];
  const realWorker = globalThis.Worker;
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  const restore = stubProbeEnvironment(
    (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(bothBusesDocument);
      if (url.includes("/probe")) return Response.json(pathProbeDocument);
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch,
    { location: "https://meter.test/", protocol: "h2" },
  );
  try {
    globalThis.Worker = PingBusWorker as unknown as typeof Worker;
    globals.WebTransport = class {};
    const { RealBackend } = await import("./RealRunner");
    const config = probeConfig(true);
    config.stages.download = false;
    config.transports.throughputTarget = "https://meter.test";
    const backend = new RealBackend();
    backend.attach(testHost(config));
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
          samples: pingSamples(2),
        });
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    expect((await degrading).selectedLatencyTransport).toBe("websocket");
    const throughputRole = await backend.probe(config, undefined, "throughput");
    expect(throughputRole.selectedLatencyTransport).toBe("websocket");
    backend.onRunStart(config);
    backend.onStageBegin(phaseActivity("latency"));
    expect(PingBusWorker.starts.at(-1)).toBe("websocket");
    backend.dispose();
  } finally {
    globalThis.Worker = realWorker;
    if (realWebTransport === undefined)
      Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = realWebTransport;
    restore();
  }
}, 15000);

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
  expect(kindsForRole("throughput")).toEqual([
    "fetch-stream",
    "webtransport",
    "webtransport-datagram",
  ]);
  expect(kindsForRole("latency")).toEqual(["websocket", "webtransport"]);
});

test("the datagram setting does not reach selection or dispatch", () => {
  const origin = "https://meter:7249";
  const catalog = discovery(
    [fetchAd(origin), dgAd(origin)],
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
