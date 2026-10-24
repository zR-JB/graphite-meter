// Net test fakes: page time that moves on demand, a network reporting Resource Timing hops, and latency buses.
import { jest, spyOn } from "bun:test";
import type { CatalogEntry } from "../../protocol/documents";
import { delay } from "../run/clock";
import type {
  OpenProbeChannel,
  ProbeChannelEvents,
  ProbeChannelOptions,
  TestConfig,
} from "../types";
import { initialAuth } from "./auth";
import type { PageConfig, PageSession } from "./page";

export const CONFIG: TestConfig = {
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: {
    warmupMs: 800,
    latencyMs: 4_000,
    downloadMs: 10_000,
    uploadMs: 10_000,
    bidirectionalMs: 10_000,
  },
  pingCadence: "reply-driven",
  loadedPingCadence: "medium",
  transferStreams: { mode: "auto", count: 4 },
  experimentalDatagramThroughput: false,
  transports: { throughputTarget: "auto", latencyTarget: "auto" },
  adaptive: true,
  visualization: { throughputMaxBytesPerSec: "auto" },
};

export const page: PageConfig = {
  origin: "https://meter.example",
  secure: true,
  auth: false,
  historyDefault: false,
  webTransport: "available",
};

export const publicAuth = (entry: CatalogEntry) => initialAuth(entry, page, {} as PageSession);

export const MARKER = { "Graphite-Meter-Auth": "required" };
export const PROBE = {
  clientIp: "192.0.2.1",
  clientIpVersion: 4,
  clientIpSource: "socket",
  protocolNegotiated: "h2",
};

export const h = (baseUrl: string, protocol: string) => ({
  baseUrl,
  transport: "fetch-stream",
  protocol,
});
export const wt = (baseUrl: string, transport = "webtransport") => ({
  baseUrl,
  transport,
  protocol: "http3",
});
export const ws = (baseUrl: string) => ({ baseUrl, transport: "websocket" });
export const preflight = (throughput: object[], latency: object[] = []) => ({
  server: { name: "Meter" },
  engineVersion: "1.0.0",
  generation: "g1",
  capabilities: { throughput, latency, uploadCheckpoint: true, maxStageMs: 300_000 },
});

/** One answer: a JSON body after `afterMs` with the hop the browser reports (h2 unless named, null for none), a hang, or unreachable. */
export type Answer =
  | {
      status?: number;
      body?: unknown;
      headers?: Record<string, string>;
      afterMs?: number;
      hop?: string | null;
    }
  | "hang"
  | "unreachable";

const settle = async () => {
  for (let turn = 0; turn < 4; turn++) await new Promise((resolve) => setImmediate(resolve));
};

/** Fake timers from page time 0 and a network answering each request as `route` says, both until `restore`. */
export function fakeNet(route: (url: URL, init: RequestInit) => Answer) {
  jest.useFakeTimers();
  Object.assign(globalThis, { BUILD: { clientVersion: "v9.9.9" } });
  const requests: { url: URL; init: RequestInit; at: number }[] = [];
  const hops = new Map<string, string>();
  let restored = false;
  const fake = async (input: RequestInfo | URL, init: RequestInit = {}) => {
    const url = new URL(String(input), page.origin);
    const signal = init.signal ?? new AbortController().signal;
    signal.throwIfAborted();
    requests.push({ url, init, at: performance.now() });
    const answer = route(url, init);
    if (answer === "unreachable") throw new TypeError("Failed to fetch");
    if (answer === "hang")
      return new Promise<never>((_, reject) =>
        signal.addEventListener("abort", () => reject(signal.reason)),
      );
    if (answer.afterMs) await delay(answer.afterMs, signal);
    signal.throwIfAborted();
    hops.set(String(input), answer.hop === undefined ? "h2" : (answer.hop ?? ""));
    const body = answer.body === undefined ? null : JSON.stringify(answer.body);
    return new Response(body, { status: answer.status ?? 200, headers: answer.headers });
  };
  const fetching = spyOn(globalThis, "fetch").mockImplementation(fake as typeof fetch);
  const timing = spyOn(performance, "getEntriesByName").mockImplementation((name) => {
    const hop = hops.get(name);
    return hop === undefined ? [] : [{ name, nextHopProtocol: hop } as PerformanceResourceTiming];
  });
  return {
    requests,
    sent: (pathname: string) =>
      requests.filter(({ url }) => url.pathname === pathname).map(({ at }) => at),
    now: () => performance.now(),
    /** Fires the timers due within `ms` in order, letting what each starts settle before the next. */
    async advance(ms = 0): Promise<void> {
      let reached = ms <= 0;
      if (!reached) setTimeout(() => (reached = true), ms);
      for (await settle(); !reached; await settle()) jest.advanceTimersToNextTimer();
    },
    restore(): void {
      if (restored) return;
      restored = true;
      fetching.mockRestore();
      timing.mockRestore();
      jest.useRealTimers();
    },
  };
}

interface Bus {
  options: ProbeChannelOptions;
  events: ProbeChannelEvents;
  /** The spacing it was opened with, or paced to since. */
  intervalMs: number | null;
  disposed: boolean;
}

/** Latency buses answering `rttMs` after measuring starts and once per interval after each reply, or never while it is null. */
export class FakeBuses {
  rttMs: number | null = 10;
  readonly opened: Bus[] = [];
  readonly open: OpenProbeChannel = (options, events) => {
    const bus: Bus = { options, events, intervalMs: options.intervalMs, disposed: false };
    this.opened.push(bus);
    let timer: ReturnType<typeof setTimeout> | undefined;
    const reply = (waitMs: number, first: boolean) => {
      timer = setTimeout(() => {
        const rttMs = this.rttMs;
        if (rttMs === null) return;
        if (first) bus.events.ready();
        bus.events.outcomes([
          {
            rttMs,
            timedOut: false,
            handlingMs: null,
            observedAt: performance.now(),
            inWindow: true,
          },
        ]);
        reply(bus.intervalMs ?? rttMs, false);
      }, waitMs);
    };
    return {
      measure: () => {
        if (this.rttMs !== null) reply(this.rttMs, true);
      },
      pace: (intervalMs) => (bus.intervalMs = intervalMs),
      stop: async () => clearTimeout(timer),
      dispose() {
        bus.disposed = true;
        clearTimeout(timer);
      },
      renew() {},
      adopt(next) {
        if (bus.disposed) return false;
        bus.events = next;
        return true;
      },
    };
  };
}
