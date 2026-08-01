import { test, expect } from "bun:test";
import type { CoreHost } from "./core";
import type { RunnerConfig } from "./contract";
// Type only: the class itself is imported dynamically, after the build globals
// the module reads at load time are in place.
import type { RealBackend } from "./RealRunner";

const dials: string[] = [];
// Refuses every dial, the shape of a network that does not carry UDP.
class FakeWebTransport {
  readonly ready: Promise<void>;
  readonly closed: Promise<void>;
  constructor(url: string) {
    dials.push(url);
    const refuse = (): Promise<never> =>
      new Promise((_, reject) =>
        queueMicrotask(() => reject(new Error("no udp here"))),
      );
    this.ready = refuse();
    this.closed = refuse();
  }
  close(): void {}
}

const WT_ORIGIN = "https://meter.test";

const preflight = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [
      { baseUrl: WT_ORIGIN, transport: "webtransport", protocol: "http3" },
    ],
    latency: [],
  },
};

const config: RunnerConfig = {
  stages: {
    latency: false,
    download: true,
    upload: false,
    bidirectional: false,
  },
  skipLoadedLatencyWhenStageOff: true,
  transports: { throughputTarget: `${WT_ORIGIN}::wt`, latencyTarget: "auto" },
  transferStreams: { mode: "auto", count: 1 },
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

test("a refused WebTransport check is re-dialled on the next probe, so Retry works", async () => {
  Object.assign(globalThis as Record<string, unknown>, {
    __GM_DEFAULT_ENGINE__: "real",
    __GM_ALLOW_DUMMY__: false,
    __GM_DEV_TOOLS__: false,
    __GM_BUILD_LABEL__: "test",
    __GM_CLIENT_VERSION__: "0.0.0-test",
  });
  const { RealBackend, TransportUnavailableError } =
    await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  try {
    globals.WebTransport = FakeWebTransport;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL(`${WT_ORIGIN}/`),
    });
    performance.getEntriesByName = () => [];
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
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;

    const backend = new RealBackend();
    backend.attach({
      config,
      phase: "idle",
      elapsed: 0,
      emit() {},
      push() {},
      stall() {},
      resume() {},
      fail() {},
      failStage() {},
    } as unknown as CoreHost);

    // An explicit ::wt selection fails its role rather than degrading, so both
    // probes reject. The point is that the second one dialled to find out.
    for (const attempt of [1, 2]) {
      await expect(backend.probe(config)).rejects.toBeInstanceOf(
        TransportUnavailableError,
      );
      expect(dials.length).toBe(attempt);
    }
    expect(dials[0]).toContain("/wt/download");
  } finally {
    globalThis.fetch = realFetch;
    performance.getEntriesByName = realEntries;
    if (realWebTransport === undefined) delete globals.WebTransport;
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
  }
});

// A handshake only proves the path reaches the server. If the first lane never
// carries a byte the run's first request would fail, so the check has to fail
// too rather than reporting the path Ready.
test("a session that establishes but carries no bytes is not Ready", async () => {
  // A session that came up holds a server admission slot until it is closed,
  // and the check runs again on every draft change, visibility return and run
  // start, so the failure path has to release it too.
  let closes = 0;
  class SilentWebTransport {
    readonly ready = Promise.resolve();
    readonly closed = new Promise<void>(() => {});
    readonly incomingUnidirectionalStreams = new ReadableStream({
      start(controller) {
        controller.close(); // established, then never opens a lane
      },
    });
    close(): void {
      closes++;
    }
  }
  Object.assign(globalThis as Record<string, unknown>, {
    __GM_DEFAULT_ENGINE__: "real",
    __GM_ALLOW_DUMMY__: false,
    __GM_DEV_TOOLS__: false,
    __GM_BUILD_LABEL__: "test",
    __GM_CLIENT_VERSION__: "0.0.0-test",
  });
  const { RealBackend, TransportUnavailableError } =
    await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  try {
    globals.WebTransport = SilentWebTransport;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL(`${WT_ORIGIN}/`),
    });
    performance.getEntriesByName = () => [];
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
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;

    const backend = new RealBackend();
    backend.attach({
      config,
      phase: "idle",
      elapsed: 0,
      emit() {},
      push() {},
      stall() {},
      resume() {},
      fail() {},
      failStage() {},
    } as unknown as CoreHost);

    await expect(backend.probe(config)).rejects.toThrow(/carried no bytes/);
    await expect(backend.probe(config)).rejects.toBeInstanceOf(
      TransportUnavailableError,
    );
    expect(closes).toBe(2); // one per established session, both released
  } finally {
    globalThis.fetch = realFetch;
    performance.getEntriesByName = realEntries;
    if (realWebTransport === undefined) delete globals.WebTransport;
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
  }
});

// The guard in front of a session dial has to test the kind that was actually
// advertised. Both WebTransport rows share one `usable` today, so a literal
// reads correct; a kind whose API this client lacks would be dialled anyway.
test("a session kind this client cannot drive fails its role before any dial", async () => {
  Object.assign(globalThis as Record<string, unknown>, {
    __GM_DEFAULT_ENGINE__: "real",
    __GM_ALLOW_DUMMY__: false,
    __GM_DEV_TOOLS__: false,
    __GM_BUILD_LABEL__: "test",
    __GM_CLIENT_VERSION__: "0.0.0-test",
  });
  const { RealBackend } = await import("./RealRunner");
  const { TRANSPORTS } = await import("./real/transports");
  const datagramPreflight = {
    ...preflight,
    capabilities: {
      throughput: [
        {
          baseUrl: WT_ORIGIN,
          transport: "webtransport-datagram",
          protocol: "http3",
        },
      ],
      latency: [],
    },
  };
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  const realUsable = TRANSPORTS["webtransport-datagram"].usable;
  try {
    // The session API is present, so the streams kind stays drivable; only the
    // datagram kind is not, which is the split a literal cannot see.
    globals.WebTransport = FakeWebTransport;
    TRANSPORTS["webtransport-datagram"].usable = () => false;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL(`${WT_ORIGIN}/`),
    });
    performance.getEntriesByName = () => [];
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(datagramPreflight);
      if (url.includes("/probe"))
        return Response.json({
          clientIp: "127.0.0.1",
          clientIpVersion: 4,
          clientIpSource: "socket",
          protocolNegotiated: "h3",
        });
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;

    const backend = new RealBackend();
    backend.attach({
      config,
      phase: "idle",
      elapsed: 0,
      emit() {},
      push() {},
      stall() {},
      resume() {},
      fail() {},
      failStage() {},
    } as unknown as CoreHost);

    const dialled = dials.length;
    await expect(
      backend.probe({
        ...config,
        transports: {
          throughputTarget: `${WT_ORIGIN}::wtdg`,
          latencyTarget: "auto",
        },
      }),
    ).rejects.toThrow(/webtransport-datagram is not supported by this client/);
    expect(dials.length).toBe(dialled);
  } finally {
    TRANSPORTS["webtransport-datagram"].usable = realUsable;
    globalThis.fetch = realFetch;
    performance.getEntriesByName = realEntries;
    if (realWebTransport === undefined) delete globals.WebTransport;
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
  }
});

/* ---------- overlapping probes ----------
 *  validateConnections aborts the running probe and starts the next one without
 *  awaiting it, so two probe() bodies can run against one backend. The role
 *  bindings they write are backend-wide. */

/** A session that establishes and then holds its verify lane until the test
 *  hands one over, so a probe can be parked inside the check. */
class HeldWebTransport {
  static readonly live: HeldWebTransport[] = [];
  readonly ready = Promise.resolve();
  readonly closed = new Promise<void>(() => {});
  readonly incomingUnidirectionalStreams: ReadableStream;
  #lanes!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;

  constructor() {
    HeldWebTransport.live.push(this);
    this.incomingUnidirectionalStreams = new ReadableStream({
      start: (controller) => {
        this.#lanes = controller;
      },
    });
  }

  /** Open one lane carrying a byte: what the check waits for. */
  deliver(): void {
    this.#lanes.enqueue(
      new ReadableStream({
        start(controller) {
          controller.enqueue(new Uint8Array(1));
          controller.close();
        },
      }),
    );
  }

  close(): void {
    this.#lanes.error(new Error("session closed"));
  }
}

const autoConfig: RunnerConfig = {
  ...config,
  transports: { throughputTarget: "auto", latencyTarget: "auto" },
};

/** Run `body` against a backend whose only advertised throughput target is the
 *  held WebTransport session, which automatic selection then commits to. */
async function withHeldSessions(
  body: (backend: RealBackend) => Promise<void>,
): Promise<void> {
  Object.assign(globalThis as Record<string, unknown>, {
    __GM_DEFAULT_ENGINE__: "real",
    __GM_ALLOW_DUMMY__: false,
    __GM_DEV_TOOLS__: false,
    __GM_BUILD_LABEL__: "test",
    __GM_CLIENT_VERSION__: "0.0.0-test",
  });
  const { RealBackend } = await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  HeldWebTransport.live.length = 0;
  try {
    globals.WebTransport = HeldWebTransport;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL(`${WT_ORIGIN}/`),
    });
    performance.getEntriesByName = () => [];
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
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;

    const backend = new RealBackend();
    backend.attach({
      config: autoConfig,
      phase: "idle",
      elapsed: 0,
      emit() {},
      push() {},
      stall() {},
      resume() {},
      fail() {},
      failStage() {},
    } as unknown as CoreHost);
    await body(backend);
  } finally {
    globalThis.fetch = realFetch;
    performance.getEntriesByName = realEntries;
    if (realWebTransport === undefined) delete globals.WebTransport;
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
  }
}

/** Turn the queue until `dials` sessions have been opened. Every fake resolves
 *  immediately, so turns are the only thing a probe waits on. */
async function untilDialled(dials: number): Promise<void> {
  for (let turn = 0; turn < 100 && HeldWebTransport.live.length < dials; turn++)
    await Promise.resolve();
  expect(HeldWebTransport.live).toHaveLength(dials);
}

// Every other await in probe() throws on abort. Reporting a verdict instead
// let an aborted check degrade an automatic selection to fetch-stream and
// resolve, as if the dial had answered.
test("an aborted WebTransport check aborts the probe, it does not degrade it", async () => {
  await withHeldSessions(async (backend) => {
    const abort = new AbortController();
    const probe = backend.probe(autoConfig, abort.signal);
    await untilDialled(1);

    abort.abort();
    expect(
      await probe.then(
        (info) => info.selectedThroughputTransport,
        () => "rejected",
      ),
    ).toBe("rejected");
  });
});

// The abort lands while the older probe is inside the dial, which is the
// longest await in probe(). Swallowing it there let that probe walk on into the
// commit and clear the session target the newer probe had already bound,
// reporting fetch-stream for a run selected onto WebTransport.
test("an aborted probe leaves the transport a newer probe committed alone", async () => {
  await withHeldSessions(async (backend) => {
    const abort = new AbortController();
    const first = backend.probe(autoConfig, abort.signal);
    await untilDialled(1);
    const second = backend.probe(autoConfig);
    await untilDialled(2);

    abort.abort();
    const firstOutcome = await first.then(
      () => "resolved",
      () => "rejected",
    );
    HeldWebTransport.live[1].deliver();

    expect((await second).selectedThroughputTransport).toBe("webtransport");
    expect(firstOutcome).toBe("rejected");
  });
});

// Supersession without an abort: the older probe's dial succeeds after the
// newer one has already committed. Its remaining writes belong to bindings it
// no longer owns, so it stops at the next await instead.
test("a probe superseded mid-dial does not commit behind the newer one", async () => {
  await withHeldSessions(async (backend) => {
    const first = backend.probe(autoConfig);
    await untilDialled(1);
    const second = backend.probe(autoConfig);
    await untilDialled(2);

    HeldWebTransport.live[1].deliver();
    expect((await second).selectedThroughputTransport).toBe("webtransport");

    HeldWebTransport.live[0].deliver();
    expect(
      await first.then(
        () => "resolved",
        () => "rejected",
      ),
    ).toBe("rejected");
  });
});
