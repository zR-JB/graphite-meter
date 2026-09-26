import { stubGlobals } from "../../test-helpers.testutil";
import { test, expect, jest, spyOn } from "bun:test";
import {
  httpToWs,
  needsPings,
  laneStaggerMs,
  protocolFromNextHop,
  selectTarget,
  browserProtocolMatchesTarget,
  classifyTransportDiscovery,
  fetchViewOfOrigin,
  candidates,
  emptyConnectionValidation,
  type AnyTarget,
} from "../paths";
import type { DiscoveredTarget } from "../contract";
import { isLoopbackHostname } from "../../servers/catalog";
import type {
  PreparedPaths,
  ConnectionRole,
  PhaseActivity,
  RunnerConfig,
  TransportDiscovery,
} from "../contract";
import { DEFAULT_CONFIG } from "../../state/defaults";
import {
  TEST_BUILD_TOKENS,
  testParticipantHost,
  testTransfer,
} from "../test-helpers.testutil";
type ThroughputAdvertisement = Parameters<
  typeof classifyTransportDiscovery
>[0][number];
type LatencyAdvertisement = Parameters<
  typeof classifyTransportDiscovery
>[1][number];
const targetOfKind = <T extends AnyTarget>(
  entry: DiscoveredTarget<T> | undefined,
  kind: string,
) => entry?.targets.find((target) => target.transport === kind);
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
  Array.from({ length: 5 }, () => ({ rtt, timedOut: false }));

test("proxy endpoints resolve relative to preflight and negotiate the browser hop", () => {
  const catalog = discovery(
    [{ baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" }],
    [{ baseUrl: ".", transport: "websocket" }],
    "https://meter.example",
    true,
    "h2",
  );
  const target = selectTarget(catalog, "throughput", "auto", true);
  expect(target?.origin).toBe("https://meter.example");
  expect(target?.protocol).toBe("negotiated");
  if (target?.transport !== "fetch-stream") throw new Error("not fetch");
  expect(browserProtocolMatchesTarget(target, "h2")).toBe(true);
  expect(selectTarget(catalog, "latency", "auto", false)?.origin).toBe(
    "https://meter.example",
  );
});

test("Automatic prefers HTTP1 bulk streams deterministically and never selects unreliable throughput", () => {
  const offered = [
    fetchAd("https://meter", "http1"),
    fetchAd("https://meter:2", "http2"),
    fetchAd("https://meter:3", "http3"),
    wtAd("https://meter:3"),
    dgAd("https://meter:3"),
  ];
  const catalog = discovery(
    offered,
    [wsAd("https://meter"), wtLatencyAd("https://meter:3")],
    "https://meter",
    true,
  );
  const ids = candidates(catalog, "throughput", true).map(
    (target) => target.id,
  );
  expect(ids).toEqual([
    "https://meter",
    "https://meter:2",
    "https://meter:3",
    "https://meter:3::wt",
  ]);
  expect(
    candidates(
      discovery(offered.toReversed(), [], "https://meter", true),
      "throughput",
      true,
    ).map((target) => target.id),
  ).toEqual(ids);
  expect(
    candidates(catalog, "latency", true).map((target) => target.transport),
  ).toEqual(["webtransport", "websocket"]);
  expect(
    candidates(catalog, "latency", false).map((target) => target.transport),
  ).toEqual(["websocket"]);
});

test("multiple off-origin alternatives remain usable and a proven proxy hop participates in ranking", () => {
  const catalog = discovery(
    [
      fetchAd("https://b:3", "http3"),
      fetchAd("https://a:3", "http3"),
      fetchAd(".", "negotiated"),
    ],
    [],
    "https://proxy",
    true,
    "h2",
  );
  expect(selectTarget(catalog, "throughput", "auto", true)?.origin).toBe(
    "https://proxy",
  );
  catalog.pageProtocol = "h3";
  expect(selectTarget(catalog, "throughput", "auto", true)?.origin).toBe(
    "https://proxy",
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
  expect(selectTarget(catalog, "throughput", "auto", true)?.protocol).toBe(
    "http1",
  );
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
  expect(
    selectTarget(catalog, "throughput", "https://meter:7248", true)?.protocol,
  ).toBe("http2");
  const h2Target = selectTarget(
    catalog,
    "throughput",
    "https://meter:7248",
    true,
  );
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
  expect(selectTarget(catalog, "throughput", origin, true)?.transport).toBe(
    "fetch-stream",
  );
  expect(
    selectTarget(catalog, "throughput", `${origin}::wt`, true)?.transport,
  ).toBe("webtransport");
  expect(
    selectTarget(catalog, "latency", `${origin}::wt`, true)?.transport,
  ).toBe("webtransport");
  expect(selectTarget(catalog, "latency", origin, true)?.transport).toBe(
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
  expect(datagram?.id).toBe("https://meter:7249::wtdg");
  expect(datagram?.transport).toBe("webtransport-datagram");
  expect(selectTarget(catalog, "throughput", "auto", true)?.transport).toBe(
    "fetch-stream",
  );
  expect(
    selectTarget(catalog, "throughput", "https://meter:7249::wt", true)
      ?.transport,
  ).toBe("webtransport");
  expect(
    selectTarget(catalog, "throughput", "https://meter:7249::wtdg", true)
      ?.transport,
  ).toBe("webtransport-datagram");
  expect(
    selectTarget(catalog, "throughput", "https://meter:7249", true)?.transport,
  ).toBe("fetch-stream");
  expect(selectTarget(catalog, "latency", "auto", false)?.transport).toBe(
    "websocket",
  );
  expect(
    selectTarget(catalog, "latency", "https://meter:7249", false),
  ).toBeNull();
  const wt = selectTarget(catalog, "latency", "auto", true);
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
  expect(selectTarget(catalog, "latency", "auto", true)?.transport).toBe(
    "webtransport",
  );
  expect(selectTarget(catalog, "latency", "auto", false)?.transport).toBe(
    "websocket",
  );
  expect(
    selectTarget(catalog, "latency", "https://meter::wt", true)?.transport,
  ).toBe("webtransport");
  expect(
    selectTarget(catalog, "latency", "https://meter", false)?.transport,
  ).toBe("websocket");
});

test("a WebTransport-only origin is auto's last resort and keeps a fetch view", () => {
  const catalog = discovery(
    [wtAd("https://meter:7249")],
    [],
    "https://ui.example",
    true,
    "h3",
  );
  const target = selectTarget(catalog, "throughput", "auto", true);
  expect(target?.transport).toBe("webtransport");
  if (target?.transport !== "webtransport") throw new Error("not wt");
  const view = fetchViewOfOrigin(catalog, target);
  expect(view.transport).toBe("fetch-stream");
  expect(view.origin).toBe("https://meter:7249");
});

test("an explicit WebSocket target resolves to a WebSocket bus", () => {
  const catalog = discovery(
    [fetchAd(".", "negotiated")],
    [{ baseUrl: ".", transport: "websocket" }],
    "https://meter.test",
    true,
  );
  expect(selectTarget(catalog, "latency", "auto", false)?.transport).toBe(
    "websocket",
  );
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

test("clear DNS and IPv4 loopback targets stay usable from HTTPS", () => {
  for (const host of ["localhost", "meter.localhost", "127.42.0.9"]) {
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
  expect(isLoopbackHostname("[::1]")).toBe(true);
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
  const { prepareConnections } = await import("./prepare");
  let validation = emptyConnectionValidation();
  let idle: import("./prepare").ConnectionPreparation["idle"];
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

const preflightDocument = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [fetchAd("http://meter.test:7246", "http1")],
    latency: [wsAd("http://meter.test:7246")],
  },
};

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

test("Automatic falls back to a verified advertised path, while explicit HTTP1 remains strict", async () => {
  const requests: string[] = [];
  const document = {
    ...preflightDocument,
    capabilities: {
      throughput: [
        fetchAd("http://meter.test:7246", "http1"),
        fetchAd("https://meter.test:7248", "http2"),
      ],
      latency: [],
    },
  };
  const restore = stubProbeEnvironment(
    (async (input) => {
      const url = String(input);
      if (url.includes("/probe")) {
        requests.push(new URL(url).origin);
        if (url.includes(":7246")) throw new TypeError("Failed to fetch");
      }
      return probeFetch(document)(input);
    }) as typeof fetch,
    { protocol: "h2" },
  );
  try {
    const harness = await preparationHarness();
    const config = {
      ...probeConfig(false),
      transports: { throughputTarget: "auto", latencyTarget: "auto" },
    };
    const paths = await harness.check(config, ["throughput"]);
    expect(requests).toEqual([
      "http://meter.test:7246",
      "https://meter.test:7248",
    ]);
    expect(paths.throughput.requested.protocol).toBe("http1");
    expect(paths.throughput.target.origin).toBe("https://meter.test:7248");
    requests.length = 0;
    config.transports.throughputTarget = "protocol:http1";
    await expect(harness.check(config, ["throughput"])).rejects.toThrow();
    expect(requests).toEqual(["http://meter.test:7246"]);
  } finally {
    restore();
  }
});

test("an unresponsive HTTP1 candidate cannot prevent Automatic from trying HTTP2", async () => {
  const originalTimeout = globalThis.setTimeout;
  const timer = spyOn(globalThis, "setTimeout").mockImplementation(((
    ...[run, delay, ...args]: Parameters<typeof setTimeout>
  ) =>
    originalTimeout(
      run,
      delay === 2000 ? 1 : delay,
      ...args,
    )) as typeof setTimeout);
  let candidateSignal: AbortSignal | undefined;
  const document = {
    ...preflightDocument,
    capabilities: {
      throughput: [
        fetchAd("http://meter.test:7246", "http1"),
        fetchAd("https://meter.test:7248", "http2"),
      ],
      latency: [],
    },
  };
  const restore = stubProbeEnvironment(
    (async (input, init) => {
      if (String(input).includes(":7246") && String(input).includes("/probe")) {
        candidateSignal = init?.signal ?? undefined;
        return new Promise<Response>(() => {});
      }
      return probeFetch(document)(input);
    }) as typeof fetch,
    { protocol: "h2" },
  );
  try {
    const harness = await preparationHarness();
    const paths = await harness.check(
      {
        ...probeConfig(false),
        transports: { throughputTarget: "auto", latencyTarget: "auto" },
      },
      ["throughput"],
    );
    expect(candidateSignal?.aborted).toBe(true);
    expect(paths.throughput.target.origin).toBe("https://meter.test:7248");
  } finally {
    restore();
    timer.mockRestore();
  }
});

test("HTTP3 bootstrap allows time for the browser upgrade instead of exhausting rapid probes", async () => {
  let attempts = 0;
  let first = 0;
  const document = {
    ...preflightDocument,
    capabilities: {
      throughput: [fetchAd("https://meter.test:7249", "http3")],
      latency: [],
    },
  };
  const restore = stubProbeEnvironment(
    (async (input) => {
      if (String(input).includes("/probe")) {
        attempts++;
        first ||= performance.now();
      }
      return probeFetch(document)(input);
    }) as typeof fetch,
    {
      protocol: () =>
        first && performance.now() - first >= 200 ? "h3" : "http/1.1",
    },
  );
  try {
    const harness = await preparationHarness();
    const paths = await harness.check(
      {
        ...probeConfig(false),
        transports: {
          throughputTarget: "protocol:http3",
          latencyTarget: "auto",
        },
      },
      ["throughput"],
    );
    expect(attempts).toBeGreaterThan(3);
    expect(paths.throughput.browserProtocol).toBe("h3");
    expect(paths.throughput.fetch.protocol).toBe("http3");
  } finally {
    restore();
  }
});

test("WebTransport verifies bytes independently of its HTTP control probe protocol", async () => {
  const restoreTransport = stubGlobals({
    WebTransport: class {
      ready = Promise.resolve();
      closed = Promise.resolve({});
      incomingUnidirectionalStreams = new ReadableStream({
        start(controller) {
          controller.enqueue(
            new ReadableStream({
              start(bytes) {
                bytes.enqueue(new Uint8Array([1]));
                bytes.close();
              },
            }),
          );
          controller.close();
        },
      });
      close() {}
    },
  });
  const document = {
    ...preflightDocument,
    capabilities: {
      throughput: [
        fetchAd("https://meter.test:7249", "http3"),
        wtAd("https://meter.test:7249"),
      ],
      latency: [],
    },
  };
  const restore = stubProbeEnvironment(probeFetch(document));
  try {
    const harness = await preparationHarness();
    const paths = await harness.check(
      {
        ...probeConfig(false),
        transports: {
          throughputTarget: "transport:webtransport",
          latencyTarget: "auto",
        },
      },
      ["throughput"],
    );
    expect(paths.throughput.target.transport).toBe("webtransport");
    expect(paths.throughput.fetch.protocol).toBe("http1");
  } finally {
    restore();
    restoreTransport();
  }
});

test("a failed WebTransport-only path cannot turn its HTTP control probe into a fetch transfer", async () => {
  const restoreTransport = stubGlobals({
    WebTransport: class {
      ready = Promise.reject(new Error("QUIC unavailable"));
      closed = Promise.resolve({});
      close() {}
    },
  });
  const document = {
    ...preflightDocument,
    capabilities: {
      throughput: [wtAd("https://meter.test:7249")],
      latency: [],
    },
  };
  const restore = stubProbeEnvironment(probeFetch(document));
  try {
    const harness = await preparationHarness();
    await expect(
      harness.check(
        {
          ...probeConfig(false),
          transports: { throughputTarget: "auto", latencyTarget: "auto" },
        },
        ["throughput"],
      ),
    ).rejects.toThrow("webtransport session did not establish");
  } finally {
    restore();
    restoreTransport();
  }
});

test("Automatic stops at an authentication failure instead of probing another endpoint", async () => {
  const { ServerAuthenticationRequired } =
    await import("../../servers/credentials");
  const requests: string[] = [];
  const document = {
    ...preflightDocument,
    capabilities: {
      throughput: [
        fetchAd("http://meter.test:7246", "http1"),
        fetchAd("https://meter.test:7249", "http3"),
      ],
      latency: [],
    },
  };
  const restore = stubProbeEnvironment((async (input) => {
    if (String(input).includes("/probe")) {
      requests.push(String(input));
      throw new ServerAuthenticationRequired({
        id: "peer",
        name: "Peer",
        url: "https://meter.test:7249",
      });
    }
    return probeFetch(document)(input);
  }) as typeof fetch);
  try {
    const harness = await preparationHarness();
    await expect(
      harness.check(
        {
          ...probeConfig(false),
          transports: { throughputTarget: "auto", latencyTarget: "auto" },
        },
        ["throughput"],
      ),
    ).rejects.toThrow();
    expect(requests).toHaveLength(1);
  } finally {
    restore();
  }
});

test("cross-origin IPv6 discovery and path preparation fail with DNS guidance before any request", async () => {
  let requests = 0;
  const restore = stubProbeEnvironment((async (
    _input: RequestInfo | URL,
  ): Promise<Response> => {
    requests++;
    throw new Error("unexpected fetch");
  }) as typeof fetch);
  try {
    const { discoverServer, prepareConnections } = await import("./prepare");
    const { BrowserOriginBlockedError } = await import("./prepare");
    const remote = "http://[::1]:7246";
    await expect(
      discoverServer(new AbortController().signal, {
        server: { id: "ipv6", name: "IPv6", url: remote },
        kind: "public",
      }),
    ).rejects.toBeInstanceOf(BrowserOriginBlockedError);
    const known = classifyTransportDiscovery(
      [fetchAd(remote, "http1")],
      [],
      remote,
      false,
      undefined,
      location.origin,
    );
    const prepared = await prepareConnections(
      DEFAULT_CONFIG,
      emptyConnectionValidation(),
      ["throughput"],
      new AbortController().signal,
      undefined,
      known,
    );
    expect(prepared.failure).toBeInstanceOf(BrowserOriginBlockedError);
    expect(prepared.validation.throughput.message).toContain("DNS hostname");
    expect(requests).toBe(0);
  } finally {
    restore();
  }
});

test("secure interfaces reject clear non-loopback discovery before any request", async () => {
  let requests = 0;
  const restore = stubProbeEnvironment(
    (async (_input: RequestInfo | URL): Promise<Response> => {
      requests++;
      throw new Error("unexpected fetch");
    }) as typeof fetch,
    { location: "https://ui.example/" },
  );
  try {
    const { discoverServer } = await import("./prepare");
    const { BrowserOriginBlockedError } = await import("./prepare");
    const failure = await discoverServer(new AbortController().signal, {
      server: {
        id: "clear",
        name: "Clear server",
        url: "http://meter.example:7246",
      },
      kind: "public",
    }).then(
      () => undefined,
      (cause: unknown) => cause,
    );
    expect(failure).toBeInstanceOf(BrowserOriginBlockedError);
    expect(failure).toMatchObject({
      message:
        "Use an HTTPS origin for this server when the interface is HTTPS.",
    });
    expect(requests).toBe(0);
  } finally {
    restore();
  }
});

test("same-origin IPv6 discovery remains available through the page origin", async () => {
  const origin = "http://[::1]:7246";
  const restore = stubProbeEnvironment(
    probeFetch({
      ...preflightDocument,
      capabilities: {
        throughput: [fetchAd(".", "http1")],
        latency: [wsAd(".")],
      },
    }),
    { location: `${origin}/` },
  );
  try {
    const { discoverServer } = await import("./prepare");
    const result = await discoverServer(new AbortController().signal, {
      server: { id: "self", name: "IPv6", url: origin },
      kind: "public",
    });
    expect(selectTarget(result, "throughput", "auto", true)?.origin).toBe(
      origin,
    );
    expect(selectTarget(result, "latency", "auto", false)?.origin).toBe(origin);
  } finally {
    restore();
  }
});

test("catalogue preflight timing includes the complete response body without probing paths", async () => {
  let now = 100;
  let requests = 0;
  const realNow = performance.now;
  const restore = stubProbeEnvironment((async (_input: RequestInfo | URL) => {
    requests++;
    now = 110;
    return new Response(
      new ReadableStream<Uint8Array>({
        pull(controller) {
          now = 145;
          controller.enqueue(
            new TextEncoder().encode(JSON.stringify(preflightDocument)),
          );
          controller.close();
        },
      }),
    );
  }) as typeof fetch);
  performance.now = () => now;
  try {
    const { discoverServer } = await import("./prepare");
    const result = await discoverServer(new AbortController().signal);
    expect(result.preflightMs).toBe(45);
    expect(requests).toBe(1);
    expect(result.server.name).toBe("test");
  } finally {
    performance.now = realNow;
    restore();
  }
});

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
  expect(selectTarget(catalog, "throughput", "auto", true)?.transport).toBe(
    "webtransport",
  );
  expect(selectTarget(catalog, "throughput", "auto", false)).toBeNull();
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
    jest.useFakeTimers();
    try {
      let settled = false;
      const pending = preparation.check(probeConfig(true)).finally(() => {
        settled = true;
      });
      // A silent bus resolves on its reply deadline, not on elapsed test time.
      for (let turn = 0; turn < 100 && !settled; turn++) {
        const worker = FakePingWorker.all.at(-1);
        worker?.emit({ type: "ready" });
        if (rtt !== null)
          worker?.emit({ type: "samples", samples: pingSamples(rtt) });
        jest.advanceTimersByTime(20);
        for (let i = 0; i < 20; i++) await Promise.resolve();
      }
      expect((await pending).latency!.rttMs).toBe(rtt);
    } finally {
      jest.useRealTimers();
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
  jest.useFakeTimers();
  try {
    globalThis.Worker = PingBusWorker as unknown as typeof Worker;
    globals.WebTransport = class {};
    const { ServerStage } = await import("../transport");
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
      jest.advanceTimersByTime(5);
      for (let i = 0; i < 20; i++) await Promise.resolve();
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
    const stage = new ServerStage({
      host: testParticipantHost(config),
      paths: throughputRole,
      activity: phaseActivity("latency"),
      streams: { down: 1, up: 1 },
      seed: "test",
    });
    void stage.prepare();
    expect(PingBusWorker.starts.at(-1)).toBe("websocket");
    stage.discard();
  } finally {
    jest.useRealTimers();
    globalThis.Worker = realWorker;
    if (realWebTransport === undefined)
      Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = realWebTransport;
    restore();
  }
});

test("latency preparation collects replies while metadata is still pending", async () => {
  FakePingWorker.all = [];
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let probes = 0;
  const fetchNormally = probeFetch();
  const restore = stubProbeEnvironment((async (input, init) => {
    if (String(input).includes("/probe") && ++probes === 2) await held;
    return fetchNormally(input, init);
  }) as typeof fetch);
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakePingWorker as unknown as typeof Worker;
  const preparation = await preparationHarness();
  try {
    const pending = preparation.check(probeConfig(true));
    for (let i = 0; i < 100 && probes < 2; i++) {
      FakePingWorker.all.at(-1)?.emit({ type: "ready" });
      await Promise.resolve();
    }
    expect(probes).toBe(2);
    FakePingWorker.all.at(-1)!.emit({ type: "ready" });
    // Allow readiness to start collection, then deliver every sample before metadata.
    for (let i = 0; i < 10; i++) await Promise.resolve();
    FakePingWorker.all
      .at(-1)!
      .emit({ type: "samples", samples: pingSamples(7) });
    release();
    expect((await pending).latency!.rttMs).toBe(7);
  } finally {
    release();
    preparation.stop();
    globalThis.Worker = realWorker;
    restore();
  }
});
