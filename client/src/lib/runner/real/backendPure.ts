/**
 * RealRunner's pure helpers: origin/URL mapping, small math, and stage-activity
 * queries with no fetch/worker/websocket entanglement. They live apart from
 * RealRunner.ts so they are unit-testable without its build-time BUILD defines.
 */

import type {
  PhaseActivity,
  ProtocolTarget,
  TransportDiscovery,
  TransportKind,
  ThroughputTargetSelection,
} from "../contract";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../../api/endpoints";
import type { LatencyEndpoint, ThroughputEndpoint } from "../../api/preflight";
import { normalizeHttpProtocol } from "../protocol";

/** Server route paths, the TS half of a cross-language pin. Preflight advertises
 *  origins and transports only, so Go keeps its own table
 *  (go/internal/server/listeners.go, go/internal/wire/preflight.go). Both halves
 *  assert against api/routes.txt (routes.test.ts, routes_test.go). */
export const ROUTES = {
  probe: "/probe",
  download: "/download",
  upload: "/upload",
  uploadSession: "/upload/session",
  uploadProgress: "/upload/progress",
  wtSession: "/wt/session",
  ping: "/ws/ping",
  wtDownload: "/wt/download",
  wtUpload: "/wt/upload",
  wtPing: "/wt/ping",
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

/** Classify the logical server catalog once for both selection and settings.
 *  Entries are keyed by origin; a WebTransport throughput endpoint folds onto
 *  its origin's entry as `wt`, since it is the same server reached another way. */
export function classifyTransportDiscovery(
  throughputEndpoints: (
    ThroughputEndpoint | FetchThroughputTarget | WebTransportThroughputTarget
  )[],
  latencyEndpoints: (LatencyEndpoint | LatencyTarget)[],
  pageOrigin: string,
  pageSecure: boolean,
  pageProtocol?: string,
): TransportDiscovery {
  const resolve = (endpoint: { baseUrl?: string; origin?: string }): string => {
    const baseUrl = endpoint.baseUrl ?? endpoint.origin ?? ".";
    return baseUrl === "." ? pageOrigin : new URL(baseUrl, pageOrigin).origin;
  };
  const stateOf = (origin: string): "advertised" | "browser-blocked" =>
    usableFromPage(origin, origin.startsWith("https://"), pageSecure)
      ? "advertised"
      : "browser-blocked";

  const throughput: TransportDiscovery["throughput"] = {};
  for (const endpoint of throughputEndpoints) {
    const origin = resolve(endpoint);
    const tls = origin.startsWith("https://");
    const entry = (throughput[origin] ??= { state: stateOf(origin) });
    if (
      endpoint.transport === "webtransport" ||
      endpoint.transport === "webtransport-datagram"
    ) {
      const streams = endpoint.transport === "webtransport";
      const view: WebTransportThroughputTarget = {
        ...endpoint,
        id: `${origin}${streams ? WT_SELECTION_SUFFIX : WT_DATAGRAM_SELECTION_SUFFIX}`,
        origin,
        transport: endpoint.transport,
        protocol: "http3",
        tls,
        routes: {
          probe: ROUTES.probe,
          wtSession: ROUTES.wtSession,
          wtDownload: ROUTES.wtDownload,
          wtUpload: ROUTES.wtUpload,
          uploadSession: ROUTES.uploadSession,
          uploadProgress: ROUTES.uploadProgress,
        },
      };
      if (streams) entry.wt = view;
      else entry.wtDatagram = view;
      continue;
    }
    // A target naming its protocol outranks a negotiated one: selection can only
    // act on a named protocol.
    if (entry.target && entry.target.protocol !== "negotiated") continue;
    entry.target = {
      ...endpoint,
      id: origin,
      origin,
      transport: "fetch-stream",
      tls,
      routes: {
        probe: ROUTES.probe,
        download: ROUTES.download,
        upload: ROUTES.upload,
        uploadSession: ROUTES.uploadSession,
        uploadProgress: ROUTES.uploadProgress,
      },
    };
  }

  const latency: TransportDiscovery["latency"] = {};
  for (const endpoint of latencyEndpoints) {
    const origin = resolve(endpoint);
    const tls = origin.startsWith("https://");
    const entry = (latency[origin] ??= { state: stateOf(origin) });
    // One origin commonly advertises both buses (a proxy serving TCP and UDP
    // on one hostname), so they are kept side by side: a client that cannot
    // reach UDP still has the WebSocket view of that origin.
    if (endpoint.transport === "webtransport") {
      entry.wt = {
        ...endpoint,
        id: `${origin}${WT_SELECTION_SUFFIX}`,
        origin,
        transport: "webtransport",
        protocol: "http3",
        tls,
        routes: {
          probe: ROUTES.probe,
          wtSession: ROUTES.wtSession,
          wtPing: ROUTES.wtPing,
        },
      };
      continue;
    }
    entry.target = {
      ...endpoint,
      id: origin,
      origin,
      transport: "websocket",
      protocol: "http1",
      tls,
      routes: { probe: ROUTES.probe, ping: ROUTES.ping },
    };
  }

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

/** Suffixes marking the WebTransport views of an origin in selection ids and
 *  the picker, since one origin advertises several mechanisms. */
export const WT_SELECTION_SUFFIX = "::wt";
export const WT_DATAGRAM_SELECTION_SUFFIX = "::wtdg";

/** Whether a throughput transport rides a WebTransport session rather than
 *  fetch requests. The switch is exhaustive by construction: a new
 *  TransportKind fails to compile here until it is classified, so the transfer
 *  path can never silently treat an unhandled kind as fetch. */
export function throughputRidesSession(kind: TransportKind): boolean {
  switch (kind) {
    case "webtransport":
    case "webtransport-datagram":
      return true;
    case "fetch-stream":
    case "websocket":
      return false;
    default: {
      const unclassified: never = kind;
      throw new Error(`unclassified transport ${String(unclassified)}`);
    }
  }
}

/** Resolve one bulk transfer path. Target ids distinguish clear and TLS H1;
 *  protocol evidence disambiguates multiple targets sharing an origin. An
 *  `origin::wt` selection names that origin's WebTransport view; auto prefers
 *  fetch and takes a lone WebTransport-only origin as the last resort. */
export function selectThroughputTarget(
  discovery: TransportDiscovery,
  selection: ThroughputTargetSelection,
): FetchThroughputTarget | WebTransportThroughputTarget | null {
  if (selection.endsWith(WT_DATAGRAM_SELECTION_SUFFIX)) {
    const entry =
      discovery.throughput[
        selection.slice(0, -WT_DATAGRAM_SELECTION_SUFFIX.length)
      ];
    return entry?.state === "advertised" ? (entry.wtDatagram ?? null) : null;
  }
  if (selection.endsWith(WT_SELECTION_SUFFIX)) {
    const entry =
      discovery.throughput[selection.slice(0, -WT_SELECTION_SUFFIX.length)];
    return entry?.state === "advertised" ? (entry.wt ?? null) : null;
  }
  if (selection !== "current" && selection !== "auto") {
    const entry = discovery.throughput[selection];
    return entry?.state === "advertised" ? (entry.target ?? null) : null;
  }
  const advertised = Object.values(discovery.throughput)
    .filter((entry) => entry.state === "advertised" && entry.target)
    .map((entry) => entry.target!);
  const fetch =
    advertised.find((target) => target.origin === discovery.pageOrigin) ??
    (advertised.length === 1 ? advertised[0] : null);
  if (fetch) return fetch;
  const wtOnly = Object.values(discovery.throughput).filter(
    (entry) => entry.state === "advertised" && !entry.target && entry.wt,
  );
  return wtOnly.length === 1 ? wtOnly[0].wt! : null;
}

/** The fetch-stream view of a WebTransport-only origin: its HTTP routes serve
 *  probe, upload minting, and the fetch fallback lanes. */
export function fetchViewOfWebTransport(
  target: WebTransportThroughputTarget,
): FetchThroughputTarget {
  return {
    id: target.origin,
    origin: target.origin,
    transport: "fetch-stream",
    protocol: "http3",
    tls: target.tls,
    routes: {
      probe: ROUTES.probe,
      download: ROUTES.download,
      upload: ROUTES.upload,
      uploadSession: ROUTES.uploadSession,
      uploadProgress: ROUTES.uploadProgress,
    },
  };
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
  target: FetchThroughputTarget | WebTransportThroughputTarget | null,
): string {
  return target ? `${target.id}\n${target.origin}` : "";
}

/** Select latency independently. An `origin::wt` selection names that origin's
 *  datagram bus; a plain origin its WebSocket bus, falling back to the datagram
 *  one for an origin that advertises nothing else. Auto prefers WebTransport,
 *  whose datagram bus measures real loss, then follows page security rather
 *  than throughput. `webTransport` is what this client can actually drive. */
export function selectLatencyTarget(
  discovery: TransportDiscovery,
  selection: "auto" | string,
  webTransport = false,
): LatencyTarget | null {
  const runnable = (target?: LatencyTarget): boolean =>
    !!target && (webTransport || target.transport !== "webtransport");
  const pick = (target?: LatencyTarget): LatencyTarget | null =>
    runnable(target) ? target! : null;
  if (selection !== "auto") {
    if (selection.endsWith(WT_SELECTION_SUFFIX)) {
      const entry =
        discovery.latency[selection.slice(0, -WT_SELECTION_SUFFIX.length)];
      return entry?.state === "advertised" ? pick(entry.wt) : null;
    }
    const entry = discovery.latency[selection];
    if (entry?.state !== "advertised") return null;
    return pick(entry.target) ?? pick(entry.wt);
  }
  const advertised = Object.values(discovery.latency).filter(
    (entry) => entry.state === "advertised",
  );
  // Each bus resolves by the same rule: the page's own origin, else the only
  // candidate. Guessing between several origins is what an explicit selection
  // is for, on either bus.
  const only = (
    targets: (LatencyTarget | undefined)[],
  ): LatencyTarget | null => {
    const usable = targets.filter(runnable) as LatencyTarget[];
    return (
      usable.find((target) => target.origin === discovery.pageOrigin) ??
      (usable.length === 1 ? usable[0] : null)
    );
  };
  return (
    only(advertised.map((entry) => entry.wt)) ??
    only(advertised.map((entry) => entry.target))
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
