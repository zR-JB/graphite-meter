import { test, expect } from "bun:test";
import type { RunnerConfig } from "./contract";
import type { RealBackend } from "./RealRunner";
import {
  TEST_BUILD_TOKENS,
  TEST_WT_ORIGIN,
  TEST_WT_PREFLIGHT,
  testHost,
  testWtConfig,
} from "./test-helpers.test";
const dials: string[] = [];
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
const WT_ORIGIN = TEST_WT_ORIGIN;
const preflight = {
  ...TEST_WT_PREFLIGHT,
  capabilities: { ...TEST_WT_PREFLIGHT.capabilities, latency: [] },
};
const config: RunnerConfig = testWtConfig({
  latency: false,
  download: true,
  upload: false,
  bidirectional: false,
});
config.transferStreams = { mode: "auto", count: 1 };
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
  Object.assign(globals, TEST_BUILD_TOKENS);
  const { RealBackend } = await import("./RealRunner");
  const realFetch = globalThis.fetch;
  const realLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const realEntries = performance.getEntriesByName.bind(performance);
  const realWebTransport = globals.WebTransport;
  const timings: PerformanceResourceTiming[] = [];
  try {
    globals.WebTransport = webTransport;
    Object.defineProperty(globalThis, "location", {
      configurable: true,
      value: new URL(`${WT_ORIGIN}/`),
    });
    performance.getEntriesByName = (name) =>
      timings.filter((entry) => entry.name === name);
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(capabilities);
      if (url.includes("/probe")) {
        const response = Response.json({
          clientIp: "127.0.0.1",
          clientIpVersion: 4,
          clientIpSource: "socket",
          protocolNegotiated: "h3",
        });
        Object.defineProperty(response, "url", { value: url });
        timings.push({
          name: url,
          nextHopProtocol: "h3",
        } as PerformanceResourceTiming);
        return response;
      }
      throw new Error(`unexpected fetch ${url}`);
    }) as typeof fetch;
    const backend = new RealBackend();
    backend.attach(testHost(probeConfig));
    await body(backend);
  } finally {
    globalThis.fetch = realFetch;
    performance.getEntriesByName = realEntries;
    if (realWebTransport === undefined)
      Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = realWebTransport;
    if (realLocation)
      Object.defineProperty(globalThis, "location", realLocation);
    for (const key of Object.keys(TEST_BUILD_TOKENS))
      Reflect.deleteProperty(globals, key);
  }
}
const withFakeProbe = (
  body: BackendBody,
  capabilities = preflight,
  probeConfig = config,
): Promise<void> =>
  withProbeBackend(FakeWebTransport, probeConfig, capabilities, body);
test("a refused WebTransport check is re-dialled on the next probe, so Retry works", async () => {
  await withFakeProbe(async (backend) => {
    const { TransportUnavailableError } = await import("./RealRunner");
    for (const attempt of [1, 2]) {
      await expect(backend.probe(config)).rejects.toBeInstanceOf(
        TransportUnavailableError,
      );
      expect(dials.length).toBe(attempt);
    }
    expect(dials[0]).toContain("/wt/download");
  });
});
test("a session that establishes but carries no bytes is not Ready", async () => {
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
class HeldWebTransport {
  static readonly live: HeldWebTransport[] = [];
  static nextDial = Promise.withResolvers<void>();
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
    HeldWebTransport.nextDial.resolve();
    HeldWebTransport.nextDial = Promise.withResolvers<void>();
  }
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
async function withHeldSessions(
  body: (backend: RealBackend) => Promise<void>,
): Promise<void> {
  HeldWebTransport.live.length = 0;
  HeldWebTransport.nextDial = Promise.withResolvers<void>();
  await withProbeBackend(HeldWebTransport, autoConfig, preflight, body);
}
async function untilDialled(dials: number): Promise<void> {
  while (HeldWebTransport.live.length < dials)
    await HeldWebTransport.nextDial.promise;
  expect(HeldWebTransport.live).toHaveLength(dials);
}
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
