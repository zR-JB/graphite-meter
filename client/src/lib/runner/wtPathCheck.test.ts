import { test, expect } from "bun:test";
import type { CoreHost } from "./core";
import type { RunnerConfig } from "./contract";
// Type only: the class itself is imported dynamically, after the build globals the module reads at load time are in.
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

const BUILD_TOKENS = {
  __GM_ALLOW_DUMMY__: false,
  __GM_BUILD_PROFILE__: "test",
  __GM_RELEASE_VERSION__: null,
  __GM_SOURCE_REVISION__: "test-revision",
  __GM_BUILD_IDENTITY__: "test test-revision",
  __GM_CLIENT_VERSION__: "0.0.0-test",
};

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

type BackendBody = (
  backend: import("./RealRunner").RealBackend,
) => Promise<void>;

async function withProbeBackend(
  webTransport: unknown,
  probeConfig: RunnerConfig,
  capabilities = preflight,
  body: BackendBody,
): Promise<void> {
  const globals = globalThis as Record<string, unknown>;
  Object.assign(globals, BUILD_TOKENS);
  const { RealBackend } = await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const realWebTransport = globals.WebTransport;
  try {
    globals.WebTransport = webTransport;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL(`${WT_ORIGIN}/`),
    });
    performance.getEntriesByName = () => [];
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(capabilities);
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
      config: probeConfig,
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
    if (realWebTransport === undefined)
      Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
  }
}

test("a refused WebTransport check is re-dialled on the next probe, so Retry works", async () => {
  await withProbeBackend(
    FakeWebTransport,
    config,
    preflight,
    async (backend) => {
      const { TransportUnavailableError } = await import("./RealRunner");
      // An explicit ::wt selection fails its role rather than degrading, so both probes reject.
      for (const attempt of [1, 2]) {
        await expect(backend.probe(config)).rejects.toBeInstanceOf(
          TransportUnavailableError,
        );
        expect(dials.length).toBe(attempt);
      }
      expect(dials[0]).toContain("/wt/download");
    },
  );
});

// A handshake only proves the path reaches the server.
test("a session that establishes but carries no bytes is not Ready", async () => {
  // A session that came up holds a server admission slot until it is closed, and the check runs again on every draft.
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
  await withProbeBackend(
    SilentWebTransport,
    config,
    preflight,
    async (backend) => {
      const { TransportUnavailableError } = await import("./RealRunner");
      await expect(backend.probe(config)).rejects.toThrow(/carried no bytes/);
      await expect(backend.probe(config)).rejects.toBeInstanceOf(
        TransportUnavailableError,
      );
      expect(closes).toBe(2); // one per established session, both released
    },
  );
});

// The guard in front of a session dial has to test the kind that was actually advertised.
test("a session kind this client cannot drive fails its role before any dial", async () => {
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
  await withProbeBackend(
    FakeWebTransport,
    config,
    datagramPreflight,
    async (backend) => {
      const { TRANSPORTS } = await import("./real/transports");
      const realUsable = TRANSPORTS["webtransport-datagram"].usable;
      try {
        // The session API is present, so the streams kind stays drivable; only the datagram kind is not, which is the.
        TRANSPORTS["webtransport-datagram"].usable = () => false;
        const dialled = dials.length;
        await expect(
          backend.probe({
            ...config,
            transports: {
              throughputTarget: `${WT_ORIGIN}::wtdg`,
              latencyTarget: "auto",
            },
          }),
        ).rejects.toThrow(
          /webtransport-datagram is not supported by this client/,
        );
        expect(dials.length).toBe(dialled);
      } finally {
        TRANSPORTS["webtransport-datagram"].usable = realUsable;
      }
    },
  );
});

/* overlapping probes ---------- validateConnections aborts the running probe and starts the next one without. */

/* A session that establishes and then holds its verify lane until the test hands one over, so a probe can be. */
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

/* Run `body` against a backend whose only advertised throughput target is the held WebTransport session, which. */
async function withHeldSessions(
  body: (backend: RealBackend) => Promise<void>,
): Promise<void> {
  HeldWebTransport.live.length = 0;
  await withProbeBackend(HeldWebTransport, autoConfig, preflight, body);
}

/* Turn the queue until `dials` sessions have been opened. */
async function untilDialled(dials: number): Promise<void> {
  for (let turn = 0; turn < 100 && HeldWebTransport.live.length < dials; turn++)
    await Promise.resolve();
  expect(HeldWebTransport.live).toHaveLength(dials);
}

// Every other await in probe() throws on abort.
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

// The abort lands while the older probe is inside the dial, which is the longest await in probe().
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

// Supersession without an abort: the older probe's dial succeeds after the newer one has already committed.
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
