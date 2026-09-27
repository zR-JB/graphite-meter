import { afterEach, expect, jest, spyOn, test } from "bun:test";
import type { RunnerConfig } from "../contract";
import {
  classifyTransportDiscovery,
  emptyConnectionValidation,
  selectTarget,
} from "../paths";
import { DEFAULT_CONFIG } from "../../state/defaults";
import { ServerAuthenticationRequired } from "../../servers/credentials";
import { settle, until } from "../../test-helpers.testutil";
import {
  TEST_WT_ORIGIN,
  testParticipantHost,
  testSelfCredentials,
  testWtConfig,
} from "../test-helpers.testutil";
import {
  fakeWebTransport,
  type FakeWebTransport,
} from "../workers/test-helpers.testutil";
import {
  dgAd,
  fetchAd,
  pathProbeDocument,
  preflightDocument,
  preflightWith,
  preparationHarness,
  probeConfig,
  probeFetch,
  stubProbeEnvironment,
  testWorkers,
  wsAd,
  wtAd,
  wtLatencyAd,
} from "./test-helpers.testutil";

let restore = () => {};
afterEach(() => {
  restore();
  restore = () => {};
  jest.useRealTimers();
});
const stub = (...args: Parameters<typeof stubProbeEnvironment>) =>
  void (restore = stubProbeEnvironment(...args));

async function drive<T>(
  pending: Promise<T>,
  each = () => {},
  turns = 10_000,
): Promise<T> {
  let settled = false;
  pending.then(
    () => (settled = true),
    () => (settled = true),
  );
  await until(() => settled || (each(), false), turns);
  return pending;
}

const auto = (config: RunnerConfig): RunnerConfig => ({
  ...config,
  transports: { throughputTarget: "auto", latencyTarget: "auto" },
});
const pingSamples = (rtt: number) =>
  Array.from({ length: 5 }, () => ({ rtt, timedOut: false }));

test("Automatic falls back to a verified advertised path, while explicit HTTP1 remains strict", async () => {
  const requests: string[] = [];
  const serve = probeFetch(
    preflightWith([
      fetchAd("http://meter.test:7246", "http1"),
      fetchAd("https://meter.test:7248", "http2"),
    ]),
  );
  stub(
    async (input) => {
      const url = String(input);
      if (url.includes("/probe")) {
        requests.push(new URL(url).origin);
        if (url.includes(":7246")) throw new TypeError("Failed to fetch");
      }
      return serve(input);
    },
    { protocol: "h2" },
  );
  const harness = await preparationHarness();
  const config = auto(probeConfig(false));
  const paths = await harness.check(config, ["throughput"]);
  expect(requests).toEqual([
    "http://meter.test:7246",
    "https://meter.test:7248",
  ]);
  expect(paths.throughput.requested.protocol).toBe("http1");
  expect(paths.throughput.target.origin).toBe("https://meter.test:7248");
  requests.length = 0;
  config.transports.throughputTarget = "protocol:http1";
  await expect(harness.check(config, ["throughput"])).rejects.toThrow(
    "http1 transport unavailable",
  );
  expect(requests).toEqual(["http://meter.test:7246"]);
});

test("an unresponsive HTTP1 candidate cannot prevent Automatic from trying HTTP2", async () => {
  let candidateSignal: AbortSignal | undefined;
  const serve = probeFetch(
    preflightWith([
      fetchAd("http://meter.test:7246", "http1"),
      fetchAd("https://meter.test:7248", "http2"),
    ]),
  );
  stub(
    async (input, init) => {
      if (String(input).includes(":7246") && String(input).includes("/probe")) {
        candidateSignal = init?.signal ?? undefined;
        return new Promise<Response>(() => {});
      }
      return serve(input);
    },
    { protocol: "h2" },
  );
  const harness = await preparationHarness();
  jest.useFakeTimers();
  const paths = await drive(
    harness.check(auto(probeConfig(false)), ["throughput"]),
  );
  expect(candidateSignal?.aborted).toBe(true);
  expect(paths.throughput.target.origin).toBe("https://meter.test:7248");
});

test("HTTP3 bootstrap allows time for the browser upgrade instead of exhausting rapid probes", async () => {
  let attempts = 0;
  let first = 0;
  const serve = probeFetch(
    preflightWith([fetchAd("https://meter.test:7249", "http3")]),
  );
  jest.useFakeTimers();
  stub(
    async (input) => {
      if (String(input).includes("/probe")) {
        attempts++;
        first ||= performance.now();
      }
      return serve(input);
    },
    {
      protocol: () =>
        first && performance.now() - first >= 200 ? "h3" : "http/1.1",
    },
  );
  const harness = await preparationHarness();
  const config = probeConfig(false);
  config.transports.throughputTarget = "protocol:http3";
  const paths = await drive(harness.check(config, ["throughput"]));
  expect(attempts).toBeGreaterThan(3);
  expect(paths.throughput.browserProtocol).toBe("h3");
  expect(paths.throughput.fetch.protocol).toBe("http3");
});

test("WebTransport verifies bytes independently of its HTTP control probe protocol", async () => {
  const { WebTransport } = fakeWebTransport((session) => {
    session.accept();
    session.lane(new Uint8Array([1]));
    session.endLanes();
  });
  stub(
    probeFetch(
      preflightWith([
        fetchAd("https://meter.test:7249", "http3"),
        wtAd("https://meter.test:7249"),
      ]),
    ),
    { globals: { WebTransport } },
  );
  const harness = await preparationHarness();
  const config = probeConfig(false);
  config.transports.throughputTarget = "transport:webtransport";
  const paths = await harness.check(config, ["throughput"]);
  expect(paths.throughput.target.transport).toBe("webtransport");
  expect(paths.throughput.fetch.protocol).toBe("http1");
});

test.each([
  ["an unavailable network", new Error("QUIC unavailable")],
  [
    "a nested sign-in refusal",
    new ServerAuthenticationRequired({
      id: "peer",
      name: "Peer",
      url: "https://meter.test:7249",
    }),
  ],
])(
  "a WebTransport-only path failing on %s never becomes a fetch transfer",
  async (_name, cause) => {
    const { WebTransport } = fakeWebTransport((session) =>
      session.refuse(cause),
    );
    stub(probeFetch(preflightWith([wtAd("https://meter.test:7249")])), {
      globals: { WebTransport },
    });
    const harness = await preparationHarness();
    await expect(
      harness.check(auto(probeConfig(false)), ["throughput"]),
    ).rejects.toThrow("webtransport session did not establish");
  },
);

test("Automatic stops at an authentication failure instead of probing another endpoint", async () => {
  const requests: string[] = [];
  const serve = probeFetch(
    preflightWith([
      fetchAd("http://meter.test:7246", "http1"),
      fetchAd("https://meter.test:7249", "http3"),
    ]),
  );
  stub(async (input) => {
    if (String(input).includes("/probe")) {
      requests.push(String(input));
      throw new ServerAuthenticationRequired({
        id: "peer",
        name: "Peer",
        url: "https://meter.test:7249",
      });
    }
    return serve(input);
  });
  const harness = await preparationHarness();
  await expect(
    harness.check(auto(probeConfig(false)), ["throughput"]),
  ).rejects.toMatchObject({
    cause: expect.any(ServerAuthenticationRequired),
  });
  expect(requests).toHaveLength(1);
});

test("cross-origin IPv6 discovery and path preparation fail with DNS guidance before any request", async () => {
  let requests = 0;
  stub(async () => {
    requests++;
    throw new Error("unexpected fetch");
  });
  const { discoverServer, prepareConnections, BrowserOriginBlockedError } =
    await import("./prepare");
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
    testSelfCredentials(),
    known,
  );
  expect(prepared.failure).toBeInstanceOf(BrowserOriginBlockedError);
  expect(prepared.validation.throughput.message).toContain("DNS hostname");
  expect(requests).toBe(0);
});

test("secure interfaces reject clear non-loopback discovery before any request", async () => {
  let requests = 0;
  stub(
    async () => {
      requests++;
      throw new Error("unexpected fetch");
    },
    { location: "https://ui.example/" },
  );
  const { discoverServer, BrowserOriginBlockedError } =
    await import("./prepare");
  const failure = await discoverServer(new AbortController().signal, {
    server: {
      id: "clear",
      name: "Clear server",
      url: "http://meter.example:7246",
    },
    kind: "public",
  }).catch((cause: unknown) => cause);
  expect(failure).toBeInstanceOf(BrowserOriginBlockedError);
  expect(failure).toMatchObject({
    message: "Use an HTTPS origin for this server when the interface is HTTPS.",
  });
  expect(requests).toBe(0);
});

test("same-origin IPv6 discovery remains available through the page origin", async () => {
  const origin = "http://[::1]:7246";
  stub(probeFetch(preflightWith([fetchAd(".", "http1")], [wsAd(".")])), {
    location: `${origin}/`,
  });
  const { discoverServer } = await import("./prepare");
  const result = await discoverServer(new AbortController().signal, {
    server: { id: "self", name: "IPv6", url: origin },
    kind: "public",
  });
  expect(selectTarget(result, "throughput", "auto", true)?.origin).toBe(origin);
  expect(selectTarget(result, "latency", "auto", false)?.origin).toBe(origin);
});

test("catalog preflight timing includes the complete response body without probing paths", async () => {
  let requests = 0;
  jest.useFakeTimers();
  stub(async () => {
    requests++;
    jest.advanceTimersByTime(10);
    return new Response(
      new ReadableStream<Uint8Array>({
        pull(controller) {
          jest.advanceTimersByTime(35);
          controller.enqueue(
            new TextEncoder().encode(JSON.stringify(preflightDocument)),
          );
          controller.close();
        },
      }),
    );
  });
  const { discoverServer } = await import("./prepare");
  const result = await discoverServer(
    new AbortController().signal,
    testSelfCredentials(),
  );
  expect(result.preflightMs).toBe(45);
  expect(requests).toBe(1);
  expect(result.server.name).toBe("test");
});

test("a client without WebTransport is refused by mechanism, not by availability", async () => {
  const catalog = classifyTransportDiscovery(
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
  for (const [advertised, location, throughputTarget, message] of [
    [
      wtAd("https://wt.meter.test"),
      "http://meter.test:7246/",
      "auto",
      /^webtransport is not supported by this client$/,
    ],
    [
      dgAd(TEST_WT_ORIGIN),
      `${TEST_WT_ORIGIN}/`,
      `${TEST_WT_ORIGIN}::wtdg`,
      /^webtransport-datagram is not supported by this client$/,
    ],
  ] as const) {
    stub(probeFetch(preflightWith([advertised])), {
      location,
      globals: { WebTransport: undefined },
    });
    const preparation = await preparationHarness(["https://wt.meter.test"]);
    const config = auto(probeConfig(false));
    config.transports.throughputTarget = throughputTarget;
    await expect(preparation.check(config)).rejects.toThrow(message);
    restore();
  }
});

test("a superseded probe does not publish its discovery", async () => {
  const first = Promise.withResolvers<void>();
  let preflights = 0;
  const serve = probeFetch();
  stub(async (input) => {
    if (String(input).includes("/preflight") && ++preflights === 1)
      await first.promise;
    return serve(input);
  });
  const preparation = await preparationHarness();
  const abort = new AbortController();
  const superseded = preparation.check(
    probeConfig(false),
    ["throughput"],
    abort.signal,
  );
  await until(() => preflights === 1);
  abort.abort(new Error("preparation superseded"));
  const prepared = await preparation.check(probeConfig(false), ["throughput"]);
  expect(prepared.discovery.generation).toBe(preflightDocument.generation);
  first.resolve();
  await expect(superseded).rejects.toThrow("preparation superseded");
  expect(preparation.discoveries).toHaveLength(1);
});

test.each([null, 0])(
  "preflight RTT preserves %s as distinct missing or zero evidence",
  async (rtt) => {
    const workers = testWorkers();
    stub(probeFetch(), { globals: { Worker: workers.Worker } });
    const preparation = await preparationHarness();
    jest.useFakeTimers();
    // A silent bus resolves on its reply deadline, not on elapsed test time.
    const paths = await drive(preparation.check(probeConfig(true)), () => {
      workers.last()?.emit({ type: "ready" });
      if (rtt !== null)
        workers.last()?.emit({ type: "samples", samples: pingSamples(rtt) });
    });
    expect(paths.latency!.rttMs).toBe(rtt);
    preparation.stop();
  },
);

test("the preparation owner can park and restart the returned idle monitor", async () => {
  const workers = testWorkers();
  stub(probeFetch(), { globals: { Worker: workers.Worker } });
  const preparation = await preparationHarness();
  await drive(preparation.check(probeConfig(true)), () => {
    workers.last()?.emit({ type: "ready" });
    workers.last()?.emit({ type: "samples", samples: pingSamples(3) });
  });
  const connectivity: string[] = [];
  preparation.observe((event) => {
    if (event.type === "connectivity") connectivity.push(event.state);
  });
  preparation.stop();
  const parked = workers.last();
  expect(parked.terminated).toBe(1);
  const replayed = [...connectivity];
  parked.emit({ type: "stall", detail: "webtransport closed" });
  expect(connectivity).toEqual(replayed);
  const started = workers.all.length;
  preparation.start();
  expect(workers.all).toHaveLength(started + 1);
  preparation.stop();
});

test("a throughput-role probe keeps the latency bus the last check committed to", async () => {
  const buses: string[] = [];
  const workers = testWorkers((worker, message) => {
    if (message.type !== "start") return;
    buses.push(String(message.transport));
    if (message.transport === "websocket")
      queueMicrotask(() => worker.emit({ type: "ready" }));
  });
  const probed: string[] = [];
  const serve = probeFetch(
    preflightWith(
      [fetchAd("https://meter.test", "http2")],
      [wsAd("https://fallback.test"), wtLatencyAd("https://meter.test")],
    ),
  );
  stub(
    async (input) => {
      const url = String(input);
      if (!url.includes("/probe")) return serve(input);
      probed.push(new URL(url).origin);
      return Response.json({
        ...pathProbeDocument,
        clientIp: url.startsWith("https://fallback.test/")
          ? "192.0.2.9"
          : "127.0.0.1",
      });
    },
    {
      location: "https://meter.test/",
      protocol: "h2",
      globals: { Worker: workers.Worker, WebTransport: class {} },
    },
  );
  jest.useFakeTimers();
  const { ServerStage } = await import("../transport");
  const config = probeConfig(true);
  config.stages.download = false;
  config.transports.throughputTarget = "https://meter.test";
  const preparation = await preparationHarness(["https://fallback.test"]);
  const fallback = await drive(preparation.check(config), () => {
    if (buses.at(-1) === "websocket")
      workers.last().emit({ type: "samples", samples: pingSamples(2) });
  });
  expect(fallback.latency!.target).toMatchObject({
    transport: "websocket",
    origin: "https://fallback.test",
  });
  expect(fallback.latency!.probe.clientIp).toBe("192.0.2.9");
  expect(probed).toContain("https://fallback.test");
  const throughputRole = await preparation.check(config, ["throughput"]);
  expect(throughputRole.latency!.target.transport).toBe("websocket");
  preparation.stop();
  const stage = new ServerStage({
    host: testParticipantHost(config),
    paths: throughputRole,
    activity: { stage: "latency", transfer: [], loadedLatency: false },
    streams: { down: 1, up: 1 },
    seed: "test",
  });
  void stage.prepare();
  expect(buses.at(-1)).toBe("websocket");
  stage.discard();
});

test("latency preparation collects replies while metadata is still pending", async () => {
  const metadata = Promise.withResolvers<void>();
  let probes = 0;
  const serve = probeFetch();
  const workers = testWorkers();
  stub(
    async (input, init) => {
      if (String(input).includes("/probe") && ++probes === 2)
        await metadata.promise;
      return serve(input, init);
    },
    { globals: { Worker: workers.Worker } },
  );
  const preparation = await preparationHarness();
  const pending = preparation.check(probeConfig(true));
  await until(() => (workers.last()?.emit({ type: "ready" }), probes === 2));
  workers.last().emit({ type: "ready" });
  // Readiness starts collection; every sample arrives before metadata.
  await settle();
  workers.last().emit({ type: "samples", samples: pingSamples(7) });
  metadata.resolve();
  expect((await pending).latency!.rttMs).toBe(7);
  preparation.stop();
});

const wtConfig = testWtConfig({
  latency: false,
  download: true,
  upload: false,
  bidirectional: false,
});
wtConfig.transferStreams = { mode: "auto", count: 1 };
const wtPreflight = preflightWith([wtAd(TEST_WT_ORIGIN)]);

async function wtPreparation(
  open?: (session: FakeWebTransport) => void,
  preflight: object = wtPreflight,
) {
  const transport = fakeWebTransport(open);
  stub(probeFetch(preflight), {
    location: `${TEST_WT_ORIGIN}/`,
    protocol: "h3",
    globals: { WebTransport: transport.WebTransport },
  });
  const preparation = await preparationHarness();
  return { sessions: transport.sessions, check: preparation.check };
}

test("a refused WebTransport check is re-dialled on the next probe, so Retry works", async () => {
  const { sessions, check } = await wtPreparation((session) =>
    session.refuse(new Error("no udp here")),
  );
  for (const dials of [1, 2]) {
    await expect(check(wtConfig)).rejects.toThrow(/did not establish/);
    expect(sessions).toHaveLength(dials);
  }
  expect(sessions[0].url).toContain("/wt/download");
});

test("a session that establishes but carries no bytes is not Ready and is released", async () => {
  const { sessions, check } = await wtPreparation((session) => {
    session.accept();
    session.endLanes();
  });
  await expect(check(wtConfig)).rejects.toThrow(/carried no bytes/);
  await expect(check(wtConfig)).rejects.toThrow(/carried no bytes/);
  expect(sessions.map((session) => session.closes)).toEqual([1, 1]);
});

test("an aborted WebTransport check aborts the probe, it does not degrade it", async () => {
  const { sessions, check } = await wtPreparation();
  const abort = new AbortController();
  const probe = check(auto(wtConfig), undefined, abort.signal);
  await until(() => sessions.length === 1);
  abort.abort();
  await expect(probe).rejects.toThrow();
});

test("an aborted probe leaves the transport a newer probe committed alone", async () => {
  const { sessions, check } = await wtPreparation();
  const abort = new AbortController();
  const first = check(auto(wtConfig), undefined, abort.signal);
  await until(() => sessions.length === 1);
  const second = check(auto(wtConfig));
  await until(() => sessions.length === 2);
  abort.abort();
  await expect(first).rejects.toThrow();
  sessions[1].lane(new Uint8Array(1));
  expect((await second).throughput.target.transport).toBe("webtransport");
});

test("datagram preparation requests and verifies datagram bytes, not a stream", async () => {
  let bytes = new Uint8Array([1]);
  const { sessions, check } = await wtPreparation(
    (session) => {
      session.accept();
      session.datagram(bytes);
      session.endDatagrams();
    },
    preflightWith([dgAd(TEST_WT_ORIGIN)]),
  );
  const config = { ...wtConfig };
  config.transports = {
    throughputTarget: `${TEST_WT_ORIGIN}::wtdg`,
    latencyTarget: "auto",
  };
  expect((await check(config)).throughput.target.transport).toBe(
    "webtransport-datagram",
  );
  expect(new URL(sessions[0].url).searchParams.get("datagrams")).toBe("1");
  bytes = new Uint8Array();
  await expect(check(config)).rejects.toThrow("carried no bytes");
});

test("automatic latency never retries WebSocket after authentication refusal", async () => {
  const { IdleKeepalive } = await import("./latencyChannel");
  const verify = spyOn(
    IdleKeepalive.prototype,
    "verifyReady",
  ).mockRejectedValue(
    new ServerAuthenticationRequired({
      id: "peer",
      name: "Peer",
      url: TEST_WT_ORIGIN,
    }),
  );
  try {
    const { check } = await wtPreparation(
      undefined,
      preflightWith(
        [fetchAd(TEST_WT_ORIGIN, "http3")],
        [wtLatencyAd(TEST_WT_ORIGIN), wsAd(TEST_WT_ORIGIN)],
      ),
    );
    const config = auto(wtConfig);
    config.stages = { ...config.stages, latency: true };
    await expect(check(config)).rejects.toThrow("Sign in to Peer");
    expect(verify).toHaveBeenCalledTimes(1);
  } finally {
    verify.mockRestore();
  }
});
