import { spyOn } from "bun:test";
import type { ConnectionRole, PreparedPaths, RunnerConfig } from "../contract";
import {
  classifyTransportDiscovery,
  emptyConnectionValidation,
} from "../paths";
import { DEFAULT_CONFIG } from "../../state/defaults";
import { stubGlobals } from "../../test-helpers.testutil";
import {
  TEST_BUILD_TOKENS,
  testSelfCredentials,
} from "../test-helpers.testutil";
import type { ConnectionPreparation } from "./prepare";

type Posted = { type: string; [field: string]: unknown };

/** Worker seam for channel tests; `answer` replies to what the owner posts, as the worker would. */
export class TestWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  readonly sent: Posted[] = [];
  terminated = 0;

  constructor(
    readonly url: string,
    readonly answer: (worker: TestWorker, message: Posted) => void = () => {},
  ) {}

  postMessage(message: Posted): void {
    this.sent.push(message);
    this.answer(this, message);
  }

  terminate(): void {
    this.terminated++;
  }

  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

export function testWorkers(answer?: TestWorker["answer"]) {
  const all: TestWorker[] = [];
  return {
    all,
    last: (): TestWorker => all.at(-1)!,
    Worker: class extends TestWorker {
      constructor(url: URL | string) {
        super(String(url), answer);
        all.push(this);
      }
    },
  };
}

export const probeConfig = (latency: boolean): RunnerConfig => ({
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
  adaptive: false,
});

type ThroughputAdvertisement = Parameters<
  typeof classifyTransportDiscovery
>[0][number];
type LatencyAdvertisement = Parameters<
  typeof classifyTransportDiscovery
>[1][number];
export const fetchAd = (
  baseUrl: string,
  protocol: ThroughputAdvertisement["protocol"] = "http3",
): ThroughputAdvertisement => ({
  baseUrl,
  transport: "fetch-stream",
  protocol,
});
export const wtAd = (baseUrl: string): ThroughputAdvertisement => ({
  baseUrl,
  transport: "webtransport",
  protocol: "http3",
});
export const dgAd = (baseUrl: string): ThroughputAdvertisement => ({
  baseUrl,
  transport: "webtransport-datagram",
  protocol: "http3",
});
export const wsAd = (baseUrl: string): LatencyAdvertisement => ({
  baseUrl,
  transport: "websocket",
});
export const wtLatencyAd = (baseUrl: string): LatencyAdvertisement => ({
  baseUrl,
  transport: "webtransport",
});

export const preflightDocument = {
  server: { name: "test" },
  engineVersion: "test",
  generation: "a",
  capabilities: {
    throughput: [fetchAd("http://meter.test:7246", "http1")],
    latency: [wsAd("http://meter.test:7246")],
  },
};

export const preflightWith = (
  throughput: ThroughputAdvertisement[],
  latency: LatencyAdvertisement[] = [],
) => ({ ...preflightDocument, capabilities: { throughput, latency } });

export const pathProbeDocument = {
  clientIp: "127.0.0.1",
  clientIpVersion: 4,
  clientIpSource: "socket",
  protocolNegotiated: "http/1.1",
};

type Serve = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export function probeFetch(preflight: object = preflightDocument): Serve {
  return async (input) => {
    const url = String(input);
    if (url.includes("/preflight")) return Response.json(preflight);
    if (url.includes("/probe")) return Response.json(pathProbeDocument);
    throw new Error(`unexpected fetch ${url}`);
  };
}

export function stubProbeEnvironment(
  fetchImpl: Serve,
  options: {
    location?: string;
    protocol?: string | (() => string);
    globals?: Record<string, unknown>;
  } = {},
): () => void {
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    fetch: fetchImpl,
    location: new URL(options.location ?? "http://meter.test:7246/"),
    ...options.globals,
  });
  const protocol = options.protocol ?? "http/1.1";
  const entries = spyOn(performance, "getEntriesByName").mockImplementation(
    () =>
      [
        {
          nextHopProtocol:
            typeof protocol === "function" ? protocol() : protocol,
        },
      ] as unknown as PerformanceEntryList,
  );
  return () => {
    entries.mockRestore();
    restore();
  };
}

/** Repeated checks of the page's own server that carry validation and the idle monitor forward. */
export async function preparationHarness(additionalOrigins: string[] = []) {
  const { prepareConnections } = await import("./prepare");
  let validation = emptyConnectionValidation();
  let idle: ConnectionPreparation["idle"];
  const discoveries: ConnectionPreparation["discovery"][] = [];
  return {
    discoveries,
    async check(
      config: RunnerConfig,
      roles: ConnectionRole[] = ["throughput", "latency"],
      signal = new AbortController().signal,
    ): Promise<PreparedPaths> {
      const credentials = testSelfCredentials();
      credentials.server.additionalOrigins = additionalOrigins;
      const result = await prepareConnections(
        config,
        validation,
        roles,
        signal,
        credentials,
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
        credentials,
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
