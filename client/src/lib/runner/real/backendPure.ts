/**
 * RealRunner's pure helpers: origin/URL mapping, small math, and stage-activity
 * queries with no fetch/worker/websocket entanglement. They live apart from
 * RealRunner.ts so they are unit-testable without its build-time BUILD defines.
 */

import type {
  PhaseActivity,
  ProtocolTarget,
  TransportDiscovery,
  ThroughputTargetSelection,
} from "../contract";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../../api/endpoints";
import type { LatencyEndpoint, ThroughputEndpoint } from "../../api/preflight";
import { normalizeHttpProtocol } from "../protocol";

/** Server route paths, the TS half of a cross-language pin. Preflight advertises
 *  origins only, so Go keeps its own table (go/internal/server/listeners.go,
 *  go/internal/wire/preflight.go) and both halves assert against api/routes.txt
 *  (routes.test.ts, routes_test.go). */
export const ROUTES = {
  probe: "/probe",
  download: "/download",
  upload: "/upload",
  uploadSession: "/upload/session",
  uploadProgress: "/upload/progress",
  ping: "/ws/ping",
} as const;

export function protocolFromNextHop(
  nextHopProtocol?: string,
): ProtocolTarget | undefined {
  const protocol = normalizeHttpProtocol(nextHopProtocol);
  return protocol === "negotiated" ? undefined : protocol;
}

export function isLoopbackHostname(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  if (host === "localhost" || host.endsWith(".localhost") || host === "::1")
    return true;
  const octets = host.split(".").map(Number);
  return (
    octets.length === 4 &&
    octets.every(
      (part) => Number.isInteger(part) && part >= 0 && part <= 255,
    ) &&
    octets[0] === 127
  );
}

function usableFromPage(
  origin: string,
  tls: boolean,
  pageSecure: boolean,
): boolean {
  if (!pageSecure || tls) return true;
  try {
    return isLoopbackHostname(new URL(origin).hostname);
  } catch {
    return false;
  }
}

/** Classify the logical server catalog once for both selection and settings. */
export function classifyTransportDiscovery(
  throughputEndpoints: (ThroughputEndpoint | FetchThroughputTarget)[],
  latencyEndpoints: (LatencyEndpoint | WebSocketLatencyTarget)[],
  pageOrigin: string,
  pageSecure: boolean,
  pageProtocol?: string,
): TransportDiscovery {
  const resolve = (baseUrl: string): string =>
    baseUrl === "." ? pageOrigin : new URL(baseUrl, pageOrigin).origin;
  const throughputTargets = throughputEndpoints.map((endpoint) => {
    const origin = resolve(
      endpoint.baseUrl ?? ("origin" in endpoint ? endpoint.origin : "."),
    );
    return {
      ...endpoint,
      id: origin,
      origin,
      transport: "fetch-stream" as const,
      tls: origin.startsWith("https://"),
      routes: {
        probe: ROUTES.probe,
        download: ROUTES.download,
        upload: ROUTES.upload,
        uploadSession: ROUTES.uploadSession,
        uploadProgress: ROUTES.uploadProgress,
      },
    };
  });
  const latencyTargets = latencyEndpoints.map((endpoint) => {
    const origin = resolve(
      endpoint.baseUrl ?? ("origin" in endpoint ? endpoint.origin : "."),
    );
    return {
      ...endpoint,
      id: origin,
      origin,
      transport: "websocket" as const,
      protocol: "http1" as const,
      tls: origin.startsWith("https://"),
      routes: { probe: ROUTES.probe, ping: ROUTES.ping },
    };
  });
  const throughput: TransportDiscovery["throughput"] = {};
  for (const target of throughputTargets) {
    // One entry per origin. A target naming its protocol outranks a negotiated
    // one: selection can only act on a named protocol.
    const current = throughput[target.origin]?.target;
    if (current && current.protocol !== "negotiated") continue;
    throughput[target.origin] = {
      state: usableFromPage(target.origin, target.tls, pageSecure)
        ? "advertised"
        : "browser-blocked",
      target,
    };
  }
  const latency = Object.fromEntries(
    latencyTargets.map((target) => {
      const id = target.origin;
      return [
        id,
        {
          state: usableFromPage(target.origin, target.tls, pageSecure)
            ? ("advertised" as const)
            : ("browser-blocked" as const),
          target,
        },
      ];
    }),
  );
  return {
    generation: "",
    engineVersion: "",
    server: { name: "" },
    fetchedAt: 0,
    pageOrigin,
    pageSecure,
    pageProtocol,
    throughput,
    latency,
  };
}

/** Resolve one bulk transfer path. Target ids distinguish clear and TLS H1;
 *  protocol evidence disambiguates multiple targets sharing an origin. */
export function selectThroughputTarget(
  discovery: TransportDiscovery,
  selection: ThroughputTargetSelection,
): FetchThroughputTarget | null {
  if (selection !== "current" && selection !== "auto") {
    const entry = discovery.throughput[selection];
    return entry?.state === "advertised" ? (entry.target ?? null) : null;
  }
  const advertised = Object.values(discovery.throughput)
    .filter((entry) => entry.state === "advertised" && entry.target)
    .map((entry) => entry.target!);
  return (
    advertised.find((target) => target.origin === discovery.pageOrigin) ??
    (advertised.length === 1 ? advertised[0] : null)
  );
}

export function browserProtocolMatchesTarget(
  target: FetchThroughputTarget,
  nextHopProtocol?: string,
): boolean {
  return (
    target.protocol === "negotiated" ||
    (!!nextHopProtocol &&
      protocolFromNextHop(nextHopProtocol) === target.protocol)
  );
}

export function throughputTargetKey(
  target: FetchThroughputTarget | null,
): string {
  return target ? `${target.id}\n${target.origin}` : "";
}

/** Select latency independently. Auto follows page security, not throughput. */
export function selectLatencyTarget(
  discovery: TransportDiscovery,
  selection: "auto" | string,
): WebSocketLatencyTarget | null {
  if (selection !== "auto") {
    const entry = discovery.latency[selection];
    return entry?.state === "advertised" ? (entry.target ?? null) : null;
  }
  const advertised = Object.values(discovery.latency)
    .filter((entry) => entry.state === "advertised" && entry.target)
    .map((entry) => entry.target!);
  return (
    advertised.find((target) => target.origin === discovery.pageOrigin) ??
    (advertised.length === 1 ? advertised[0] : null)
  );
}

/** Map an http(s) origin to its ws(s) equivalent for the latency bus. Anything
 *  already ws(s):// (or relative) passes through unchanged. */
export function httpToWs(origin: string): string {
  if (origin.startsWith("https://"))
    return "wss://" + origin.slice("https://".length);
  if (origin.startsWith("http://"))
    return "ws://" + origin.slice("http://".length);
  return origin;
}

/** Median of a non-empty number list (used for the pre-test ping). */
export function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Whether a stage runs a ping channel: the idle latency stage, or a transfer
 *  stage with loaded latency (bufferbloat) active. */
export function needsPings(activity: PhaseActivity): boolean {
  return (
    activity.stage === "latency" ||
    (activity.transfer.length > 0 && activity.loadedLatency)
  );
}

/** Per-lane spawn delay for `streams` parallel lanes over a `warmupMs` window.
 *  Caps at `baseMs`, and shrinks so the last lane still starts within half the
 *  warmup. Zero (spawn together) for a single lane or no warmup. */
export function laneStaggerMs(
  streams: number,
  warmupMs: number,
  baseMs: number,
): number {
  return streams > 1
    ? Math.min(baseMs, Math.floor((warmupMs * 0.5) / (streams - 1)))
    : 0;
}
