/* RealRunner's pure helpers: origin/URL mapping, small math, and stage-activity queries with no. */

import type {
  DiscoveredTarget,
  FlowDirection,
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
import { kindsForRole } from "./transports";

/* Server route paths, the TS half of a cross-language pin. */
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

/** Large enough that a normal stage ends by aborting the stream, not by refetching. */
export const PER_STREAM_BYTES = 64 * 1024 * 1024 * 1024;

/* Everything a lane URL is built from. */
export interface LaneUrlSpec {
  dir: FlowDirection;
  /** Origin of the fetch view, which every non-session lane rides. */
  base: string;
  downloadPath: string;
  uploadPath: string;
  /** Per-run cache-buster seed, suffixed with the lane index. */
  cbSeed: string;
  bytes: number;
  /** Set when this direction rides a session instead of fetch lanes. */
  session?: {
    origin: string;
    uploadPath: string;
    downloadPath: string;
    datagrams: boolean;
  } | null;
}

/* The URL lane `index` opens. */
export function laneUrl(
  spec: LaneUrlSpec,
  index: number,
  uploadId?: string,
): string {
  const { session } = spec;
  if (session)
    return `${session.origin}${session.uploadPath}?id=${encodeURIComponent(
      uploadId ?? "",
    )}${session.datagrams ? "&datagrams=1" : ""}`;
  const cb = `${spec.cbSeed}-${index}`;
  if (spec.dir === "down")
    return `${spec.base}${spec.downloadPath}?bytes=${spec.bytes}&cb=${cb}`;
  const id = uploadId ? `&id=${encodeURIComponent(uploadId)}` : "";
  return `${spec.base}${spec.uploadPath}?cb=${cb}${id}`;
}

/* The session URL a WebTransport download dials: the server opens the lanes, so the count rides the query rather. */
export function sessionDownloadUrl(
  session: NonNullable<LaneUrlSpec["session"]>,
  bytes: number,
  streams: number,
): string {
  const query = session.datagrams
    ? `?bytes=${bytes}&datagrams=1`
    : `?bytes=${bytes}&streams=${streams}`;
  return `${session.origin}${session.downloadPath}${query}`;
}

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

/* Classify the logical server catalog once for both selection and settings. */
export function classifyTransportDiscovery(
  throughputEndpoints: (
    ThroughputEndpoint | FetchThroughputTarget | WebTransportThroughputTarget
  )[],
  latencyEndpoints: (
    | (Omit<LatencyEndpoint, "transport"> & {
        transport?: LatencyEndpoint["transport"];
      })
    | LatencyTarget
  )[],
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
    // An absent one is the original contract's fetch stream, which the Go decoder assumes too; dropping it would.
    const mechanism: string = endpoint.transport ?? "fetch-stream";
    const origin = resolve(endpoint);
    const tls = origin.startsWith("https://");
    const entry = (throughput[origin] ??= {
      state: stateOf(origin),
      targets: [],
    });
    if (mechanism === "webtransport" || mechanism === "webtransport-datagram") {
      const streams = mechanism === "webtransport";
      admit(entry, {
        ...endpoint,
        id: selectionId(origin, mechanism),
        origin,
        transport: streams ? "webtransport" : "webtransport-datagram",
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
      });
      continue;
    }
    if (mechanism !== "fetch-stream") continue;
    admit(entry, {
      ...endpoint,
      id: selectionId(origin, "fetch-stream"),
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
    });
  }
  sortTargets(throughput, kindsForRole("throughput"));

  const latency: TransportDiscovery["latency"] = {};
  for (const endpoint of latencyEndpoints) {
    // An absent mechanism is the original wire contract's WebSocket bus.
    const mechanism: string = endpoint.transport ?? "websocket";
    const origin = resolve(endpoint);
    const tls = origin.startsWith("https://");
    const entry = (latency[origin] ??= { state: stateOf(origin), targets: [] });
    if (mechanism === "webtransport") {
      admit(entry, {
        ...endpoint,
        id: selectionId(origin, "webtransport"),
        origin,
        transport: "webtransport",
        protocol: "http3",
        tls,
        routes: {
          probe: ROUTES.probe,
          wtSession: ROUTES.wtSession,
          wtPing: ROUTES.wtPing,
        },
      });
      continue;
    }
    if (mechanism !== "websocket") continue;
    admit(entry, {
      ...endpoint,
      id: selectionId(origin, "websocket"),
      origin,
      transport: "websocket",
      protocol: "http1",
      tls,
      routes: { probe: ROUTES.probe, ping: ROUTES.ping },
    });
  }
  sortTargets(latency, kindsForRole("latency"));

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

/* The persisted name of one mechanism on one origin. */
function selectionId(origin: string, kind: TransportKind): string {
  switch (kind) {
    case "fetch-stream":
    case "websocket":
      return origin;
    case "webtransport":
      return `${origin}::wt`;
    case "webtransport-datagram":
      return `${origin}::wtdg`;
  }
}

/* A target naming its protocol outranks a negotiated one: selection can only act on a named protocol. */
function admit<T extends { transport: TransportKind; protocol?: string }>(
  entry: DiscoveredTarget<T>,
  target: T,
): void {
  const at = entry.targets.findIndex((x) => x.transport === target.transport);
  if (at === -1) entry.targets.push(target);
  else if (entry.targets[at].protocol === "negotiated")
    entry.targets[at] = target;
}

function sortTargets<T extends { transport: TransportKind }>(
  byOrigin: Record<string, DiscoveredTarget<T>>,
  order: TransportKind[],
): void {
  for (const entry of Object.values(byOrigin))
    entry.targets.sort(
      (a, b) => order.indexOf(a.transport) - order.indexOf(b.transport),
    );
}

/** The view one origin advertises for a mechanism. */
export function targetOfKind<T extends { transport: TransportKind }>(
  entry: DiscoveredTarget<T> | undefined,
  kind: TransportKind,
): T | undefined {
  return entry?.targets.find((target) => target.transport === kind);
}

/* Find what a selection names, and the origin carrying it, whatever that origin's state. */
export function locateTarget<T extends { id: string }>(
  byOrigin: Record<string, DiscoveredTarget<T>>,
  id: string,
): { entry: DiscoveredTarget<T>; target: T } | null {
  for (const entry of Object.values(byOrigin))
    for (const target of entry.targets)
      if (target.id === id) return { entry, target };
  return null;
}

/** What a selection resolves to, or null when its origin is not advertised. */
function advertisedById<T extends { id: string }>(
  byOrigin: Record<string, DiscoveredTarget<T>>,
  id: string,
): T | null {
  const found = locateTarget(byOrigin, id);
  return found?.entry.state === "advertised" ? found.target : null;
}

/* Resolve one bulk transfer path. */
export function selectThroughputTarget(
  discovery: TransportDiscovery,
  selection: ThroughputTargetSelection,
  webTransport = true,
): FetchThroughputTarget | WebTransportThroughputTarget | null {
  const runnable = (
    target: FetchThroughputTarget | WebTransportThroughputTarget | null,
  ): FetchThroughputTarget | WebTransportThroughputTarget | null =>
    target && (webTransport || target.transport === "fetch-stream")
      ? target
      : null;
  if (selection !== "current" && selection !== "auto")
    return runnable(advertisedById(discovery.throughput, selection));
  const advertised = Object.values(discovery.throughput).filter(
    (entry) => entry.state === "advertised",
  );
  // Automatic leads with fetch streams, which still win raw rate over TCP, so a session is the explicit choice.
  const fetch = advertised
    .map((entry) => targetOfKind(entry, "fetch-stream"))
    .filter((target) => !!target);
  const preferred =
    fetch.find((target) => target.origin === discovery.pageOrigin) ??
    (fetch.length === 1 ? fetch[0] : null);
  if (preferred) return preferred;
  // A WebTransport-only origin is the last resort, and `runnable` is what keeps it to a client that can drive the.
  const wtOnly = advertised.filter(
    (entry) =>
      !targetOfKind(entry, "fetch-stream") &&
      targetOfKind(entry, "webtransport"),
  );
  return wtOnly.length === 1
    ? runnable(targetOfKind(wtOnly[0], "webtransport")!)
    : null;
}

/* Probe evidence and the upload id are HTTP whichever mechanism moves the bytes. */
export function fetchViewOfOrigin(
  discovery: TransportDiscovery,
  target: WebTransportThroughputTarget,
): FetchThroughputTarget {
  const advertised = targetOfKind(
    discovery.throughput[target.origin],
    "fetch-stream",
  );
  return (
    (advertised as FetchThroughputTarget) ?? fetchViewOfWebTransport(target)
  );
}

/* The session names HTTP/3, but these lanes are ordinary fetches whose version the browser negotiates — often the. */
export function fetchViewOfWebTransport(
  target: WebTransportThroughputTarget,
): FetchThroughputTarget {
  return {
    id: target.origin,
    origin: target.origin,
    transport: "fetch-stream",
    protocol: "negotiated",
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

/* Select latency independently of throughput. */
export function selectLatencyTarget(
  discovery: TransportDiscovery,
  selection: "auto" | string,
  webTransport = false,
): LatencyTarget | null {
  const runnable = (target?: LatencyTarget): boolean =>
    !!target && (webTransport || target.transport !== "webtransport");
  if (selection !== "auto") {
    const named = advertisedById(discovery.latency, selection);
    if (named) return runnable(named) ? named : null;
    // A plain origin also names one that advertises no WebSocket bus at all.
    const entry = discovery.latency[selection];
    if (entry?.state !== "advertised") return null;
    const usable = entry.targets.filter(runnable);
    return usable.length === 1 ? usable[0] : null;
  }
  const advertised = Object.values(discovery.latency).filter(
    (entry) => entry.state === "advertised",
  );
  // Each bus resolves by the same rule: the page's own origin, else the only candidate.
  const only = (kind: TransportKind): LatencyTarget | null => {
    const usable = advertised
      .map((entry) => targetOfKind(entry, kind))
      .filter(runnable) as LatencyTarget[];
    return (
      usable.find((target) => target.origin === discovery.pageOrigin) ??
      (usable.length === 1 ? usable[0] : null)
    );
  };
  // Automatic prefers the datagram bus for probe timeout evidence without stream retransmission.
  return only("webtransport") ?? only("websocket");
}

/* Map an http(s) origin to its ws(s) equivalent for the latency bus. */
export function httpToWs(origin: string): string {
  if (origin.startsWith("https://"))
    return "wss://" + origin.slice("https://".length);
  if (origin.startsWith("http://"))
    return "ws://" + origin.slice("http://".length);
  return origin;
}

/* Whether a stage runs a ping channel: the idle latency stage, or a transfer stage with loaded latency. */
export function needsPings(activity: PhaseActivity): boolean {
  return (
    activity.stage === "latency" ||
    (activity.transfer.length > 0 && activity.loadedLatency)
  );
}

/* Per-lane spawn delay for `streams` parallel lanes over a `warmupMs` window. */
export function laneStaggerMs(
  streams: number,
  warmupMs: number,
  baseMs: number,
): number {
  return streams > 1
    ? Math.min(baseMs, Math.floor((warmupMs * 0.5) / (streams - 1)))
    : 0;
}
