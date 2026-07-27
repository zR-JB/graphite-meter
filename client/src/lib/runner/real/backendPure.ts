/**
 * RealRunner's pure helpers: origin/URL mapping, small math, and stage-activity
 * queries with no fetch/worker/websocket entanglement. They live apart from
 * RealRunner.ts so they are unit-testable without its build-time BUILD defines.
 */

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

/** Large enough that a normal stage ends by aborting the stream, not by refetching. */
export const PER_STREAM_BYTES = 64 * 1024 * 1024 * 1024;

/** Everything a lane URL is built from. Held here so the runner and any other
 *  driver of the same lanes cannot disagree about the shape. */
export interface LaneUrlSpec {
  dir: FlowDirection;
  /** Origin of the fetch view, which every non-session lane rides. */
  base: string;
  downloadPath: string;
  uploadPath: string;
  /** Per-run cache-buster seed, suffixed with the lane index. */
  cbSeed: string;
  bytes: number;
  /** Chunked download omits the baked-in size; the worker appends its own. */
  chunkDownload: boolean;
  /** Set when this direction rides a session instead of fetch lanes. */
  session?: {
    origin: string;
    uploadPath: string;
    downloadPath: string;
    datagrams: boolean;
  } | null;
}

/** The URL lane `index` opens. A session upload carries the minted id and no
 *  cache-buster; a session download is one URL for the whole session. */
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
    return spec.chunkDownload
      ? `${spec.base}${spec.downloadPath}?cb=${cb}`
      : `${spec.base}${spec.downloadPath}?bytes=${spec.bytes}&cb=${cb}`;
  const id = uploadId ? `&id=${encodeURIComponent(uploadId)}` : "";
  return `${spec.base}${spec.uploadPath}?cb=${cb}${id}`;
}

/** The session URL a WebTransport download dials: the server opens the lanes,
 *  so the count rides the query rather than the lane index. */
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

/** Classify the logical server catalog once for both selection and settings.
 *  Entries are keyed by origin, carrying every mechanism it advertises. */
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
    // Widened because the wire value is unvalidated JSON: a mechanism this
    // client does not know is skipped below, never renamed to one it does. An
    // absent one is the original contract's fetch stream, which the Go decoder
    // assumes too; dropping it would leave an older server unmeasurable.
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
    const mechanism: string = endpoint.transport;
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

/** The persisted name of one mechanism on one origin. Built here and matched
 *  whole everywhere else: nothing else may take an id apart. */
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

/** Add a mechanism to its origin. A target naming its protocol outranks a
 *  negotiated one: selection can only act on a named protocol. */
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

/** Find what a selection names, and the origin carrying it, whatever that
 *  origin's state. Ids are matched, never parsed. */
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

/** Resolve one bulk transfer path. A selection names one mechanism on one
 *  origin; automatic states its own preference below. `webTransport` is what
 *  this client can actually drive, which gates every session path.
 *
 *  It defaults on, unlike `selectLatencyTarget`'s: the runner resolves first and
 *  refuses second, so its "webtransport is not supported by this client" names
 *  the mechanism instead of degrading to "target unavailable". Callers that
 *  present a path rather than drive one pass the browser's real capability, so
 *  no card offers what the runner would refuse. */
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
  // Automatic leads with fetch streams, which still win raw rate over TCP, so a
  // session is the explicit choice.
  const fetch = advertised
    .map((entry) => targetOfKind(entry, "fetch-stream"))
    .filter((target) => !!target);
  const preferred =
    fetch.find((target) => target.origin === discovery.pageOrigin) ??
    (fetch.length === 1 ? fetch[0] : null);
  if (preferred) return preferred;
  // A WebTransport-only origin is the last resort, and `runnable` is what keeps
  // it to a client that can drive the session.
  const wtOnly = advertised.filter(
    (entry) =>
      !targetOfKind(entry, "fetch-stream") &&
      targetOfKind(entry, "webtransport"),
  );
  return wtOnly.length === 1
    ? runnable(targetOfKind(wtOnly[0], "webtransport")!)
    : null;
}

/** The fetch view of the origin a WebTransport target sits on: its advertised
 *  fetch target, else HTTP routes synthesised from the session one. Probe
 *  evidence and the upload id are HTTP whichever mechanism moves the bytes. */
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

/** The fetch-stream view of a WebTransport-only origin: its HTTP routes serve
 *  probe, upload minting, and the fetch fallback lanes. The session names
 *  HTTP/3, but these lanes are ordinary fetches whose version the browser
 *  negotiates — often the h2 over TCP a blocked UDP path degraded to, which is
 *  the one protocol this view must not claim. */
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

/** Select latency independently of throughput. `webTransport` is what this
 *  client can actually drive, which gates the datagram bus. */
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
  // Each bus resolves by the same rule: the page's own origin, else the only
  // candidate. Guessing between several origins is what an explicit selection
  // is for, on either bus.
  const only = (kind: TransportKind): LatencyTarget | null => {
    const usable = advertised
      .map((entry) => targetOfKind(entry, kind))
      .filter(runnable) as LatencyTarget[];
    return (
      usable.find((target) => target.origin === discovery.pageOrigin) ??
      (usable.length === 1 ? usable[0] : null)
    );
  };
  // Automatic prefers the datagram bus, whose losses are real packet loss.
  return only("webtransport") ?? only("websocket");
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
