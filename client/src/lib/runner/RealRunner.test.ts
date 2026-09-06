import { stubGlobals } from "../test-helpers.test";
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
import { kindsForRole, ridesSession } from "./real/transports";
import type {
  PreparedPaths,
  ConnectionRole,
  PhaseActivity,
  RunnerConfig,
  StallInfo,
  TransportDiscovery,
} from "./contract";
import { emptyConnectionValidation } from "./connectionModel";
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

test("an explicit WebSocket target resolves to a WebSocket bus", () => {
  const catalog = discovery(
    [fetchAd(".", "negotiated")],
    [{ baseUrl: ".", transport: "websocket" }],
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

async function preparationHarness() {
  const { prepareConnections } = await import("./real/prepare");
  let validation = emptyConnectionValidation();
  let idle: import("./real/prepare").ConnectionPreparation["idle"];
  const discoveries: TransportDiscovery[] = [];
  return {
    discoveries,
    async check(
      config: RunnerConfig,
      roles: ConnectionRole[] = ["throughput", "latency"],
      signal = new AbortController().signal,
    ): Promise<PreparedPaths> {
      const result = await prepareConnections(
        config,
        validation,
        roles,
        signal,
      );
      validation = result.validation;
      discoveries.push(result.discovery);
      if (result.idle !== undefined) {
        idle?.stop();
        idle = result.idle;
      }
      if (result.failure) throw result.failure;
      if (!validation.throughput.path)
        throw new Error("throughput path missing");
      return {
        discovery: result.discovery,
        throughput: validation.throughput.path,
        latency: validation.latency.path,
      };
    },
    stop() {
      idle?.stop();
    },
    start() {
      idle?.start();
    },
    observe(listener: NonNullable<typeof idle>["onEvent"]) {
      if (idle) idle.onEvent = listener;
    },
  };
}

test("real backend: probe refresh keeps the negotiated protocol per role, and the upload stage opens its progress channel before any POST lane", async () => {
  const realWorker = globalThis.Worker;
  const started: string[] = [];
  const workerStarts: { kind: string; url: string }[] = [];
  const pingMessages: Record<string, unknown>[] = [];
  const fetchUrls: string[] = [];
  const transferWorkers: FakeWorker[] = [];
  let preflights = 0;
  let browserProtocol = "http/1.1";
  let progressFeed: ReadableStreamDefaultController<Uint8Array> | null = null;
  const progressSignals: AbortSignal[] = [];
  const sendProgress = (record: object): void => {
    progressFeed!.enqueue(
      new TextEncoder().encode(JSON.stringify(record) + "\n"),
    );
  };
  const until = async (predicate: () => boolean): Promise<void> => {
    for (let i = 0; i < 100 && !predicate(); i++) await Bun.sleep(1);
    expect(predicate()).toBe(true);
  };
  let pingWorker: FakeWorker | null = null;
  class FakeWorker {
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: ((event: ErrorEvent) => void) | null = null;
    readonly kind: "ping" | "upload" | "download";
    constructor(url: URL) {
      const path = String(url);
      this.kind = path.includes("upload-worker")
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
      }
    }
    emit(data: unknown): void {
      this.onmessage?.({ data } as MessageEvent);
    }
    terminate(): void {}
  }
  const probeWithRtts = async (
    probe: Promise<PreparedPaths>,
    rtt: number,
  ): Promise<PreparedPaths> => {
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
    (async (input: RequestInfo | URL, init?: RequestInit) => {
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
      if (url.includes("/upload/progress")) {
        if (init?.method === "DELETE")
          return new Response(null, { status: 204 });
        started.push("progress");
        progressSignals.push(init!.signal!);
        return new Response(
          new ReadableStream<Uint8Array>({
            start(controller) {
              progressFeed = controller;
              init!.signal!.addEventListener(
                "abort",
                () => controller.error(init!.signal!.reason),
                { once: true },
              );
            },
          }),
        );
      }
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch,
    { location: "http://meter.test:7246/", protocol: () => browserProtocol },
  );
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  const preparation = await preparationHarness();
  try {
    const { RealBackend } = await import("./RealRunner");
    const { TransportUnavailableError } = await import("./real/transportError");
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
    const discoveries = preparation.discoveries;
    const uploadBytes: number[] = [];
    const stalls: StallInfo[] = [];
    const host = testHost(config, {
      failStage(_stage, _reason, message) {
        failures.push(message);
      },
      ingestLatencyAccountingIncomplete() {
        incompleteAccounting++;
      },
      ingestThroughput(_dir, bytes) {
        uploadBytes.push(bytes);
      },
      stall(info) {
        stalls.push(info);
      },
    });
    const probe = () => preparation.check(config);
    const firstProbe = await probeWithRtts(probe(), 3);
    expect(firstProbe.latency!.rttMs).toBe(3);
    config.transports.throughputTarget = "https://meter.test:7249";
    await expect(probe()).rejects.toBeInstanceOf(TransportUnavailableError);
    expect(
      discoveries.at(-1)?.throughput["https://meter.test:7248"].state,
    ).toBe("advertised");
    config.transports.throughputTarget = "http://meter.test:7246";
    const info = await probeWithRtts(probe(), 5);
    expect(
      pingMessages.filter((message) => message.type === "start").length,
    ).toBeGreaterThan(1);
    expect(info.latency!.rttMs).toBe(5);
    expect(info.latency!.probe.clientIp).toBe("127.0.0.1");
    expect(info.latency!.probe.clientIpVersion).toBe(4);
    expect(info.latency!.probe.clientIpSource).toBe("socket");
    expect(info.latency!.probe.protocolNegotiated).toBe("http/1.1");
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
    await preparation.check(config, ["throughput"]);
    expect(fetchUrls.slice(fetchStart)).toHaveLength(2);
    fetchStart = fetchUrls.length;
    const latencyProbe = await probeWithRtts(
      preparation.check(config, ["latency"]),
      7,
    );
    expect(latencyProbe.latency!.rttMs).toBe(7);
    expect(fetchUrls.slice(fetchStart)).toHaveLength(2);
    config.transports.throughputTarget = "https://proxy.test";
    browserProtocol = "";
    const proxyWithoutTiming = await preparation.check(config, ["throughput"]);
    expect(proxyWithoutTiming.throughput.fetch.protocol).toBe("negotiated");
    expect(
      targetOfKind(
        discoveries.at(-1)!.throughput["https://proxy.test"],
        "fetch-stream",
      )?.protocol,
    ).toBe("negotiated");
    browserProtocol = "h2";
    const proxyThroughput = await preparation.check(config, ["throughput"]);
    expect(proxyThroughput.throughput.fetch.protocol).toBe("http2");
    const proxyLatency = await probeWithRtts(
      preparation.check(config, ["latency"]),
      9,
    );
    expect(proxyLatency.throughput.fetch.protocol).toBe("http2");
    expect(proxyLatency.latency!.rttMs).toBe(9);
    expect(
      targetOfKind(
        discoveries.at(-1)!.throughput["https://proxy.test"],
        "fetch-stream",
      )?.protocol,
    ).toBe("negotiated");
    config.transports.throughputTarget = "http://meter.test:7246";
    browserProtocol = "http/1.1";
    const paths = await preparation.check(config, ["throughput"]);
    preparation.stop();
    const backend = new RealBackend(paths);
    backend.attach(host);
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
    const stagePreparation = backend.onStageBegin({
      stage: "upload",
      transfer: ["up"],
      loadedLatency: false,
    });
    await until(() => progressFeed !== null);
    expect(started).toEqual(["progress"]);
    expect(fetchUrls.at(-1)).toBe(
      "http://meter.test:7246/upload/progress?id=gmu_test",
    );
    sendProgress({ type: "ready" });
    await stagePreparation;
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
    sendProgress({ type: "progress", bytes: 100, nanos: 100_000_000 });
    sendProgress({ type: "progress", bytes: 200, nanos: 200_000_000 });
    await until(() => uploadBytes.length === 1);
    const ending = backend.onStageEnd(activity);
    if (!ending)
      throw new Error("upload finalization did not return a promise");
    let ended = false;
    void ending.then(() => (ended = true));
    await Promise.resolve();
    expect(ended).toBe(false);
    sendProgress({ type: "complete", bytes: 300, nanos: 300_000_000 });
    await ending;
    expect(uploadBytes).toEqual([100, 100]);
    expect(ended).toBe(true);
    uploadBytes.length = 0;
    progressFeed = null;
    const stalePreparation = backend.onStageBegin(activity);
    await until(() => progressFeed !== null);
    sendProgress({ type: "ready" });
    await stalePreparation;
    backend.onStageMeasure(activity);
    const staleEnding = backend.onStageEnd(activity);
    if (!staleEnding)
      throw new Error("stale upload finalization did not return a promise");
    backend.onAbort();
    expect(progressSignals.at(-1)?.aborted).toBe(true);
    backend.onRunStart(config);
    progressFeed = null;
    const replacementPreparation = backend.onStageBegin(activity);
    await until(() => progressFeed !== null);
    sendProgress({ type: "ready" });
    await replacementPreparation;
    backend.onStageMeasure(activity);
    await staleEnding;
    sendProgress({ type: "progress", bytes: 100, nanos: 100_000_000 });
    sendProgress({ type: "progress", bytes: 200, nanos: 200_000_000 });
    await until(() => uploadBytes.length === 1);
    expect(uploadBytes).toEqual([100]);
    backend.onComplete();
  } finally {
    preparation.stop();
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
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    fetch: fetchImpl,
    location: new URL(options.location ?? "http://meter.test:7246/"),
  });
  const realEntries = performance.getEntriesByName;
  performance.getEntriesByName = () =>
    [
      {
        nextHopProtocol:
          typeof options.protocol === "function"
            ? options.protocol()
            : (options.protocol ?? "http/1.1"),
      },
    ] as unknown as PerformanceEntry[];
  return () => {
    performance.getEntriesByName = realEntries;
    restore();
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
    const preparation = await preparationHarness();
    const config = {
      ...probeConfig(false),
      transports: { throughputTarget: "auto", latencyTarget: "auto" },
    };
    await expect(preparation.check(config)).rejects.toThrow(
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
    const preparation = await preparationHarness();
    const abort = new AbortController();
    const superseded = preparation.check(
      probeConfig(false),
      ["throughput"],
      abort.signal,
    );
    for (let turn = 0; turn < 20 && preflights < 1; turn++)
      await Promise.resolve();
    abort.abort(new Error("preparation superseded"));
    const prepared = await preparation.check(probeConfig(false), [
      "throughput",
    ]);
    expect(prepared.discovery.generation).toBe(preflightDocument.generation);
    releaseFirst();
    await expect(superseded).rejects.toThrow("preparation superseded");
    expect(preparation.discoveries).toHaveLength(1);
  } finally {
    restore();
  }
});

test.each([null, 0])(
  "preflight RTT preserves %s as distinct missing or zero evidence",
  async (rtt) => {
    FakePingWorker.all = [];
    const restore = stubProbeEnvironment(probeFetch());
    const realWorker = globalThis.Worker;
    globalThis.Worker = FakePingWorker as unknown as typeof Worker;
    const preparation = await preparationHarness();
    try {
      let settled = false;
      const pending = preparation.check(probeConfig(true)).finally(() => {
        settled = true;
      });
      for (let turn = 0; turn < 100 && !settled; turn++) {
        const worker = FakePingWorker.all.at(-1);
        worker?.emit({ type: "ready" });
        if (rtt !== null)
          worker?.emit({ type: "samples", samples: pingSamples(rtt) });
        await Promise.resolve();
      }
      expect((await pending).latency!.rttMs).toBe(rtt);
    } finally {
      preparation.stop();
      globalThis.Worker = realWorker;
      restore();
    }
  },
);

test("the preparation owner can park and restart the returned idle monitor", async () => {
  FakePingWorker.all = [];
  const restore = stubProbeEnvironment(probeFetch());
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakePingWorker as unknown as typeof Worker;
  try {
    const preparation = await preparationHarness();
    const connectivity: string[] = [];
    let settled = false;
    const probe = preparation.check(probeConfig(true)).finally(() => {
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
    preparation.observe((event) => {
      if (event.type === "connectivity") connectivity.push(event.state);
    });
    preparation.stop();
    const parked = FakePingWorker.all.at(-1)!;
    expect(parked.terminated).toBe(true);
    const emitted = connectivity.length;
    parked.emit({ type: "stall", detail: "webtransport closed" });
    expect(connectivity).toHaveLength(emitted);
    const workers = FakePingWorker.all.length;
    preparation.start();
    expect(FakePingWorker.all).toHaveLength(workers + 1);
    preparation.stop();
  } finally {
    globalThis.Worker = realWorker;
    restore();
  }
});
const bothBusesDocument = {
  ...preflightDocument,
  capabilities: {
    throughput: [fetchAd("https://meter.test", "http2")],
    latency: [wsAd("https://fallback.test"), wtLatencyAd("https://meter.test")],
  },
};

test("a throughput-role probe keeps the latency bus the last check committed to", async () => {
  PingBusWorker.live = [];
  PingBusWorker.starts = [];
  const probedUrls: string[] = [];
  const realWorker = globalThis.Worker;
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  const restore = stubProbeEnvironment(
    (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(bothBusesDocument);
      if (url.includes("/probe")) {
        probedUrls.push(url);
        return Response.json({
          ...pathProbeDocument,
          clientIp: url.startsWith("https://fallback.test/")
            ? "192.0.2.9"
            : "127.0.0.1",
        });
      }
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
    const preparation = await preparationHarness();
    let settled = false;
    const degrading = preparation.check(config).then(
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
    const fallback = await degrading;
    expect(fallback.latency!.target.transport).toBe("websocket");
    expect(fallback.latency!.target.origin).toBe("https://fallback.test");
    expect(fallback.latency!.probe.clientIp).toBe("192.0.2.9");
    expect(
      probedUrls.map((url) => new URL(url).origin + new URL(url).pathname),
    ).toContain("https://fallback.test/probe");
    const throughputRole = await preparation.check(config, ["throughput"]);
    expect(throughputRole.latency!.target.transport).toBe("websocket");
    preparation.stop();
    const backend = new RealBackend(throughputRole);
    backend.attach(testHost(config));
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

test("transport dispatch distinguishes sessions and supported roles", () => {
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
