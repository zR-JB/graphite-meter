// Connection paths: discovery classification, selection, verification state and stream plans.
import type {
  ConnectionRole,
  DiscoveredTarget,
  FlowDirection,
  PhaseActivity,
  PreparedPaths,
  ProtocolTarget,
  RunnerConfig,
  TransferStreamPolicy,
  TransportDiscovery,
  TransportKind,
  VerifiedLatencyPath,
  VerifiedThroughputPath,
} from "./contract";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../api/endpoints";
import type { LatencyEndpoint, ThroughputEndpoint } from "../api/decode";
import {
  browserOriginRestriction,
  isLoopbackHostname,
  type ServerEntry,
} from "../servers/catalog";

export type ThroughputTarget =
  FetchThroughputTarget | WebTransportThroughputTarget;
export type AnyTarget = ThroughputTarget | LatencyTarget;

/** Server route paths, the TS half of a cross-language pin. */
export const ROUTES = {
  servers: "/servers",
  uploadCheckpoint: "/upload/checkpoint",
  wsSession: "/ws/session",
  preflight: "/preflight",
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
/** The server clamps WebTransport lanes here in both directions. */
export const WT_MAX_LANES = 16;
const BROWSER_CONNECTION_BUDGET = 6;
export const MAX_STREAMS = 128;

export function normalizeHttpProtocol(
  protocol?: string,
): ProtocolTarget | undefined {
  if (protocol === "http1" || protocol === "http/1.1") return "http1";
  if (protocol === "http2" || protocol === "h2") return "http2";
  if (protocol === "http3" || protocol === "h3") return "http3";
  return protocol === "negotiated" ? "negotiated" : undefined;
}

export function httpProtocolLabel(protocol?: string): string {
  const label = {
    http1: "HTTP/1.1",
    http2: "HTTP/2",
    http3: "HTTP/3",
    negotiated: "Negotiated HTTP",
  };
  const normalized = normalizeHttpProtocol(protocol);
  return normalized ? label[normalized] : (protocol ?? "unknown");
}

export function protocolFromNextHop(
  nextHopProtocol?: string,
): ProtocolTarget | undefined {
  const protocol = normalizeHttpProtocol(nextHopProtocol);
  return protocol === "negotiated" ? undefined : protocol;
}

const hasWebTransport = (): boolean => typeof WebTransport !== "undefined";

/** Whether this client can drive a kind the server advertised. */
export const transportRunnable = (kind: TransportKind): boolean =>
  !kind.startsWith("webtransport") || hasWebTransport();

/** Why this browser cannot drive WebTransport, or null when it can. */
export function webTransportGap(): "insecure-page" | "no-api" | null {
  if (hasWebTransport()) return null;
  return globalThis.isSecureContext === false ? "insecure-page" : "no-api";
}

/** The persisted name of one mechanism on one origin. */
function selectionId(origin: string, kind: TransportKind): string {
  return kind === "webtransport"
    ? `${origin}::wt`
    : kind === "webtransport-datagram"
      ? `${origin}::wtdg`
      : origin;
}

const PICKER_ORDER: TransportKind[] = [
  "fetch-stream",
  "websocket",
  "webtransport",
  "webtransport-datagram",
];
const THROUGHPUT_KINDS = new Set<string>([
  "fetch-stream",
  "webtransport",
  "webtransport-datagram",
]);
const LATENCY_KINDS = new Set<string>(["websocket", "webtransport"]);

/** Classifies the advertised endpoints once, against the page that uses them. */
export function classifyTransportDiscovery(
  throughputEndpoints: (ThroughputEndpoint | ThroughputTarget)[],
  latencyEndpoints: (LatencyEndpoint | LatencyTarget)[],
  pageOrigin: string,
  pageSecure: boolean,
  pageProtocol?: string,
  browserOrigin = pageOrigin,
): TransportDiscovery {
  const classify = <T extends AnyTarget>(
    endpoints: {
      baseUrl?: string;
      origin?: string;
      transport: string;
      protocol?: string;
    }[],
    kinds: Set<string>,
  ): Record<string, DiscoveredTarget<T>> => {
    const byOrigin: Record<string, DiscoveredTarget<T>> = {};
    for (const endpoint of endpoints) {
      if (!kinds.has(endpoint.transport)) continue;
      const base = endpoint.baseUrl ?? endpoint.origin ?? ".";
      const origin =
        base === "." ? pageOrigin : new URL(base, pageOrigin).origin;
      const tls = origin.startsWith("https://");
      const blockedReason = browserOriginRestriction(origin, browserOrigin);
      const usable =
        !pageSecure || tls || isLoopbackHostname(new URL(origin).hostname);
      const entry = (byOrigin[origin] ??= {
        state: blockedReason || !usable ? "browser-blocked" : "advertised",
        ...(blockedReason ? { blockedReason } : {}),
        targets: [],
      });
      const transport = endpoint.transport as TransportKind;
      const protocol = transport.startsWith("webtransport")
        ? "http3"
        : transport === "websocket"
          ? "http1"
          : endpoint.protocol;
      const target = {
        id: selectionId(origin, transport),
        origin,
        transport,
        protocol,
        tls,
      } as T;
      // A target naming its protocol outranks a negotiated one.
      const at = entry.targets.findIndex(
        (known) => known.transport === transport,
      );
      if (at === -1) entry.targets.push(target);
      else if (entry.targets[at].protocol === "negotiated")
        entry.targets[at] = target;
    }
    for (const entry of Object.values(byOrigin))
      entry.targets.sort(
        (a, b) =>
          PICKER_ORDER.indexOf(a.transport) - PICKER_ORDER.indexOf(b.transport),
      );
    return byOrigin;
  };
  return {
    generation: "",
    engineVersion: "",
    server: { name: "" },
    fetchedAt: 0,
    pageOrigin,
    pageSecure,
    pageProtocol,
    throughput: classify(throughputEndpoints, THROUGHPUT_KINDS),
    latency: classify(latencyEndpoints, LATENCY_KINDS),
  };
}

type Discovered<R extends ConnectionRole> = R extends "throughput"
  ? ThroughputTarget
  : LatencyTarget;

/** What a selection names, and the origin carrying it, whatever that origin's state. */
export function locateTarget<T extends { id: string }>(
  byOrigin: Record<string, DiscoveredTarget<T>>,
  id: string,
): { entry: DiscoveredTarget<T>; target: T } | null {
  for (const entry of Object.values(byOrigin))
    for (const target of entry.targets)
      if (target.id === id) return { entry, target };
  return null;
}

function matchesGroup(target: AnyTarget, selection: string): boolean {
  if (selection.startsWith("protocol:"))
    return (
      target.transport === "fetch-stream" &&
      target.protocol === selection.slice("protocol:".length)
    );
  return target.transport === selection.slice("transport:".length);
}

const isGroup = (selection: string) =>
  selection.startsWith("protocol:") || selection.startsWith("transport:");

/** A known browser policy restriction, only when it excludes every matching target. */
export function blockedSelectionReason(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  selection: string,
): string | undefined {
  let reason: string | undefined;
  for (const entry of Object.values<DiscoveredTarget<AnyTarget>>(
    discovery[role],
  )) {
    const matches = entry.targets.some((target) =>
      selection === "auto"
        ? true
        : isGroup(selection)
          ? matchesGroup(target, selection)
          : target.id === selection || target.origin === selection,
    );
    if (!matches) continue;
    if (entry.state === "advertised") return undefined;
    reason ??= entry.blockedReason;
  }
  return reason;
}

function advertised<R extends ConnectionRole>(
  discovery: TransportDiscovery,
  role: R,
): Discovered<R>[] {
  return Object.values<DiscoveredTarget<Discovered<R>>>(
    discovery[role] as Record<string, DiscoveredTarget<Discovered<R>>>,
  ).flatMap((entry) => (entry.state === "advertised" ? entry.targets : []));
}

/** Page origin first, then TLS, then a stable id order. */
function originPreference(
  discovery: TransportDiscovery,
  a: AnyTarget,
  b: AnyTarget,
): number {
  const page = (target: AnyTarget) =>
    Number(target.origin === discovery.pageOrigin);
  return (
    page(b) - page(a) ||
    Number(b.tls) - Number(a.tls) ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  );
}

/** Automatic candidates in preference order: HTTP/1.1 bulk streams and datagram latency first; datagram throughput is always explicit. */
export function candidates<R extends ConnectionRole>(
  discovery: TransportDiscovery,
  role: R,
  webTransport = hasWebTransport(),
): Discovered<R>[] {
  const rank = (target: AnyTarget): number => {
    if (role === "latency") return target.transport === "webtransport" ? 0 : 1;
    if (target.transport !== "fetch-stream") return 4;
    const protocol =
      target.protocol === "negotiated" && target.origin === discovery.pageOrigin
        ? (protocolFromNextHop(discovery.pageProtocol) ?? "negotiated")
        : target.protocol;
    return { http1: 0, http2: 1, http3: 2, negotiated: 3 }[protocol];
  };
  return advertised(discovery, role)
    .filter(
      (target) =>
        target.transport !== "webtransport-datagram" &&
        (webTransport || target.transport !== "webtransport"),
    )
    .sort((a, b) => rank(a) - rank(b) || originPreference(discovery, a, b));
}

/** Resolves a selection exactly as a run would, or null when nothing usable matches. */
export function selectTarget<R extends ConnectionRole>(
  discovery: TransportDiscovery,
  role: R,
  selection: string,
  webTransport = hasWebTransport(),
): Discovered<R> | null {
  const runnable = (target?: AnyTarget | null) =>
    !!target && (webTransport || !target.transport.startsWith("webtransport"));
  if (selection === "auto")
    return candidates(discovery, role, webTransport)[0] ?? null;
  if (isGroup(selection)) {
    const group = advertised(discovery, role).filter((target) =>
      matchesGroup(target, selection),
    );
    const secure = discovery.pageSecure
      ? group.filter((target) => target.tls)
      : [];
    const eligible = secure.length ? secure : group;
    const target =
      eligible.find((entry) => entry.origin === discovery.pageOrigin) ??
      eligible[0];
    return runnable(target) ? target! : null;
  }
  const found = locateTarget<Discovered<R>>(
    discovery[role] as Record<string, DiscoveredTarget<Discovered<R>>>,
    selection,
  );
  if (found)
    return found.entry.state === "advertised" && runnable(found.target)
      ? found.target
      : null;
  // A plain origin also names an origin with exactly one usable mechanism.
  const entry = (
    discovery[role] as Record<string, DiscoveredTarget<Discovered<R>>>
  )[selection];
  const usable =
    entry?.state === "advertised" ? entry.targets.filter(runnable) : [];
  return usable.length === 1 ? usable[0] : null;
}

/** Probe evidence and the upload id are HTTP whichever mechanism moves the bytes. */
export function fetchViewOfOrigin(
  discovery: TransportDiscovery,
  target: ThroughputTarget,
): FetchThroughputTarget {
  const advertisedView = discovery.throughput[target.origin]?.targets.find(
    (entry): entry is FetchThroughputTarget =>
      entry.transport === "fetch-stream",
  );
  return (
    advertisedView ?? {
      id: target.origin,
      origin: target.origin,
      transport: "fetch-stream",
      protocol: "negotiated",
      tls: target.tls,
    }
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

export const httpToWs = (origin: string): string =>
  origin.replace(/^http/, "ws");

/** The idle latency stage, or a transfer stage with loaded latency, runs a ping channel. */
export const needsPings = (activity: PhaseActivity): boolean =>
  activity.stage === "latency" ||
  (activity.transfer.length > 0 && activity.loadedLatency);

/** Per-lane spawn delay spreading `streams` lanes over half the warmup. */
export const laneStaggerMs = (
  streams: number,
  warmupMs: number,
  baseMs: number,
): number =>
  streams > 1
    ? Math.min(baseMs, Math.floor((warmupMs * 0.5) / (streams - 1)))
    : 0;

/** The URL fetch lane `index` opens; each lane carries its own cache buster. */
export function laneUrl(
  spec: { dir: FlowDirection; base: string; cbSeed: string },
  index: number,
  uploadId?: string,
): string {
  const cb = `${spec.cbSeed}-${index}`;
  if (spec.dir === "down")
    return `${spec.base}${ROUTES.download}?bytes=${PER_STREAM_BYTES}&cb=${cb}`;
  return `${spec.base}${ROUTES.upload}?cb=${cb}${uploadId ? `&id=${encodeURIComponent(uploadId)}` : ""}`;
}

export const normalizeStreamCount = (count: number): number =>
  Number.isFinite(count)
    ? Math.min(MAX_STREAMS, Math.max(1, Math.round(count)))
    : 1;

/* Upload splits by protocol: under loss h2 gains from four lanes while h3 loses. */
const MULTIPLEXED: Partial<
  Record<ProtocolTarget, Record<FlowDirection, number>>
> = {
  http2: { down: 1, up: 4 },
  http3: { down: 1, up: 1 },
};

/** One server's lanes for a direction before any shared-origin budget. */
function streamCount(
  policy: TransferStreamPolicy,
  protocol: ProtocolTarget,
  activity: PhaseActivity,
  dir: FlowDirection,
  pings: boolean,
  webTransport = false,
): number {
  if (policy.mode === "forced")
    return Math.min(
      webTransport ? WT_MAX_LANES : MAX_STREAMS,
      normalizeStreamCount(policy.count),
    );
  if (webTransport) return 1;
  if (MULTIPLEXED[protocol]) return MULTIPLEXED[protocol][dir];
  const available = Math.max(
    1,
    BROWSER_CONNECTION_BUDGET -
      Number(pings) -
      Number(activity.transfer.includes("up")),
  );
  const ceiling = normalizeStreamCount(policy.count);
  if (activity.transfer.length === 1) return Math.min(available, ceiling);
  const lower = Math.floor(available / 2);
  return Math.min(
    dir === activity.transfer[0] ? available - lower : Math.max(1, lower),
    ceiling,
  );
}

export type ServerStreamPlan = Record<string, Record<FlowDirection, number>>;

/** Lanes per server and direction; HTTP/1 servers sharing an origin share one browser connection pool. */
export function planServerStreams(
  config: RunnerConfig,
  servers: readonly { id: string; paths: PreparedPaths }[],
  activity: PhaseActivity,
): ServerStreamPlan {
  const plan: ServerStreamPlan = Object.create(null);
  const h1 = new Map<string, { id: string; dir: FlowDirection }[]>();
  const control = new Map<string, number>();
  const occupy = (origin: string) =>
    control.set(origin, (control.get(origin) ?? 0) + 1);
  for (const { id, paths } of servers) {
    const { target, fetch } = paths.throughput;
    const wt = target.transport !== "fetch-stream";
    const pings = needsPings(activity) && paths.latency !== null;
    plan[id] = { down: 0, up: 0 };
    for (const dir of activity.transfer) {
      plan[id][dir] = streamCount(
        config.transferStreams,
        fetch.protocol,
        activity,
        dir,
        pings,
        wt,
      );
      if (!wt && !MULTIPLEXED[fetch.protocol])
        h1.set(fetch.origin, [...(h1.get(fetch.origin) ?? []), { id, dir }]);
    }
    if (activity.transfer.includes("up") && !wt) occupy(fetch.origin);
    if (needsPings(activity) && paths.latency?.target.transport === "websocket")
      occupy(paths.latency.target.origin);
  }
  for (const [origin, lanes] of h1) {
    const available =
      BROWSER_CONNECTION_BUDGET -
      (control.get(origin) ?? 0) -
      Number(activity.transfer.includes("up"));
    if (available < lanes.length)
      throw new Error(
        `The selected servers share ${origin}, which has insufficient HTTP/1 connection capacity for this stage`,
      );
    if (
      lanes.reduce((total, lane) => total + plan[lane.id][lane.dir], 0) <=
      available
    )
      continue;
    if (config.transferStreams.mode === "forced")
      throw new Error(
        `Forced streams would occupy the progress and control capacity at ${origin}. Reduce streams or use Automatic`,
      );
    // Every lane keeps one stream; the rest is dealt round-robin up to each ceiling.
    const ceilings = lanes.map((lane) => plan[lane.id][lane.dir]);
    for (const lane of lanes) plan[lane.id][lane.dir] = 1;
    let remaining = available - lanes.length;
    while (
      remaining > 0 &&
      lanes.some((lane, i) => plan[lane.id][lane.dir] < ceilings[i])
    )
      lanes.forEach((lane, i) => {
        if (remaining && plan[lane.id][lane.dir] < ceilings[i]) {
          plan[lane.id][lane.dir]++;
          remaining--;
        }
      });
  }
  for (const dir of activity.transfer)
    if (
      Object.values(plan).reduce((total, count) => total + count[dir], 0) >
      MAX_STREAMS
    )
      throw new Error(
        "The run exceeds 128 streams per direction. Reduce forced streams",
      );
  return plan;
}

/** The transfer activities a configuration's enabled stages run. */
export function transferActivities(config: RunnerConfig): PhaseActivity[] {
  const loadedLatency =
    !config.skipLoadedLatencyWhenStageOff || config.stages.latency;
  return (
    [
      ["download", ["down"]],
      ["upload", ["up"]],
      ["bidirectional", ["down", "up"]],
    ] as const
  ).flatMap(([stage, transfer]) =>
    config.stages[stage]
      ? [{ stage, transfer: [...transfer], loadedLatency }]
      : [],
  );
}

/** Rejects a configuration whose stream plan cannot fit before any connection opens. */
export function validateServerStreams(
  config: RunnerConfig,
  servers: readonly { id: string; paths: PreparedPaths }[],
) {
  for (const activity of transferActivities(config))
    planServerStreams(config, servers, activity);
}

/** The lane policy in words; `activities` are the stages the run will execute. */
export function describeTransferStreams(
  policy: TransferStreamPolicy,
  activities: readonly PhaseActivity[],
  protocol?: ProtocolTarget,
  transport?: TransportKind,
): string {
  if (transport === "webtransport-datagram") return "Datagram flood · no lanes";
  const forced = normalizeStreamCount(policy.count);
  if (policy.mode === "forced")
    return transport === "webtransport" && forced > WT_MAX_LANES
      ? `Forced · ${WT_MAX_LANES} per direction (capped from ${forced} by the session)`
      : `Forced · ${forced} per direction`;
  if (transport === "webtransport")
    return "Automatic · 1 continuous stream per direction";
  const lanes = protocol && MULTIPLEXED[protocol];
  if (lanes) return `Automatic · ${lanes.down} download / ${lanes.up} upload`;
  const most = Math.max(
    0,
    ...activities.flatMap((activity) =>
      activity.transfer.map((dir) =>
        streamCount(
          policy,
          protocol ?? "negotiated",
          activity,
          dir,
          needsPings(activity),
        ),
      ),
    ),
  );
  return `Automatic · up to ${most || forced} per direction`;
}

export type ConnectionValidationState =
  "checking" | "verified" | "failed" | "stale";
interface RoleValidation<Path> {
  selection: string;
  state: ConnectionValidationState;
  path: Path | null;
  message?: string;
}
export interface ConnectionValidation {
  throughput: RoleValidation<VerifiedThroughputPath>;
  latency: RoleValidation<VerifiedLatencyPath>;
}
export interface ServerView {
  readonly server: ServerEntry;
  readonly discovery: TransportDiscovery | null;
  readonly validation: ConnectionValidation;
  readonly readiness:
    "unchecked" | "checking" | "verified" | "sign-in" | "failed";
  readonly message?: string;
  /** A reason no connection check can clear: offline, sign-in or a missing server capability. */
  readonly blocked?: string;
  readonly paths: PreparedPaths | null;
  readonly metadataChecking: boolean;
}

export const emptyConnectionValidation = (): ConnectionValidation => ({
  throughput: { selection: "auto", state: "stale", path: null },
  latency: { selection: "auto", state: "stale", path: null },
});
export const CONNECTION_FRESH_MS = 2 * 60_000;
export const CONNECTION_ROLES: ConnectionRole[] = ["throughput", "latency"];
export const connectionFailureBackoff = (attempt: number): number =>
  [30_000, 60_000, 120_000, 240_000, 300_000][
    Math.max(0, Math.min(attempt - 1, 4))
  ];

export const connectionSelection = (
  config: RunnerConfig,
  role: ConnectionRole,
): string =>
  role === "throughput"
    ? config.transports.throughputTarget
    : config.transports.latencyTarget;

export const latencyPathNeeded = (config: RunnerConfig): boolean =>
  config.stages.latency ||
  (!config.skipLoadedLatencyWhenStageOff &&
    (config.stages.download ||
      config.stages.upload ||
      config.stages.bidirectional));

/** The part of a configuration that decides one role's verified path. */
export function connectionDraftRoleKey(
  config: RunnerConfig,
  role: ConnectionRole,
): string {
  const selection = connectionSelection(config, role);
  return role === "throughput"
    ? JSON.stringify({
        selection,
        checkpointNeeded: config.stages.upload || config.stages.bidirectional,
      })
    : JSON.stringify({ selection, needed: latencyPathNeeded(config) });
}

export function roleNeedsValidation(
  config: RunnerConfig,
  validation: ConnectionValidation,
  role: ConnectionRole,
  discovery?: TransportDiscovery | null,
): boolean {
  if (role === "latency" && !latencyPathNeeded(config)) return false;
  const check = validation[role];
  const selection = connectionSelection(config, role);
  const target = discovery && selectTarget(discovery, role, selection);
  const path = check.path?.target;
  return (
    check.state !== "verified" ||
    !path ||
    !discovery ||
    check.path!.generation !== discovery.generation ||
    JSON.stringify(check.path!.requested) !== JSON.stringify(target) ||
    // Automatic may keep a verified fallback; an explicit selection must use that mechanism.
    (selection !== "auto" &&
      (path.id !== target?.id ||
        path.origin !== target?.origin ||
        path.transport !== target?.transport))
  );
}

/** Upload accounting requires receiver checkpoints even when the path probe succeeded. */
export function uploadCapabilityFailure(
  config: RunnerConfig,
  discovery: Pick<TransportDiscovery, "uploadCheckpoint"> | null | undefined,
): string | undefined {
  return discovery &&
    (config.stages.upload || config.stages.bidirectional) &&
    !discovery.uploadCheckpoint
    ? "Receiver checkpoint support is required for uploads. Upgrade this measurement server."
    : undefined;
}

/** Freshness belongs to each verified role; there is no second prepared cache. */
export function preparedPaths(
  config: RunnerConfig,
  discovery: TransportDiscovery | null,
  validation: ConnectionValidation,
  maxAgeMs = CONNECTION_FRESH_MS,
): PreparedPaths | null {
  const stale = (role: ConnectionRole) =>
    roleNeedsValidation(config, validation, role, discovery) ||
    ((role === "throughput" || latencyPathNeeded(config)) &&
      Date.now() - validation[role].path!.verifiedAt > maxAgeMs);
  if (
    !discovery ||
    uploadCapabilityFailure(config, discovery) ||
    CONNECTION_ROLES.some(stale)
  )
    return null;
  return {
    discovery,
    throughput: validation.throughput.path!,
    latency: latencyPathNeeded(config) ? validation.latency.path : null,
  };
}

/** A role card reports only its own evidence across the participating servers. */
export function summarizeRoleValidation(
  role: ConnectionRole,
  ids: readonly string[],
  servers: ReadonlyMap<string, Pick<ServerView, "validation">>,
): { state: ConnectionValidationState; verified: number; total: number } {
  const states = ids.map(
    (id): ConnectionValidationState =>
      servers.get(id)?.validation[role].state ?? "stale",
  );
  const worst = (["checking", "failed", "stale"] as const).find((state) =>
    states.includes(state),
  );
  return {
    state: !states.length ? "stale" : (worst ?? "verified"),
    verified: states.filter((state) => state === "verified").length,
    total: states.length,
  };
}

/** Carries a transport preference between servers without carrying another server's origin. */
export function portableTransportSelection(
  role: ConnectionRole,
  selection: string,
  discovery: TransportDiscovery | null | undefined,
): string {
  if (selection === "auto" || isGroup(selection)) return selection;
  // Resolve with API support enabled: this converts a saved preference, not browser capability.
  const target =
    discovery &&
    (locateTarget<AnyTarget>(discovery[role], selection)?.target ??
      selectTarget(discovery, role, selection, true));
  if (!target) {
    if (selection.endsWith("::wt")) return "transport:webtransport";
    if (role === "throughput" && selection.endsWith("::wtdg"))
      return "transport:webtransport-datagram";
    return "auto";
  }
  if (target.transport !== "fetch-stream")
    return `transport:${target.transport}`;
  return target.protocol === "negotiated"
    ? "auto"
    : `protocol:${target.protocol}`;
}
