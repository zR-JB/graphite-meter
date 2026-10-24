// One stage's lanes per server and direction, from a model of each HTTP/1.1 origin's browser connection pool.
import { activities, type Activity } from "../run/schedule";
import type {
  Direction,
  FetchTarget,
  HttpProtocol,
  LatencyTarget,
  OriginTargets,
  TestConfig,
  ThroughputTarget,
} from "../types";

/** Concurrent HTTP/1.1 requests a browser runs per origin; more wait for a free connection. */
export const BROWSER_POOL = 6;
/** The server clamps a WebTransport session's lanes here in both directions. */
export const SESSION_MAX_LANES = 16;
export const MAX_STREAMS = 128;
// Upload splits by protocol: under loss HTTP/2 gains from four lanes while HTTP/3 loses.
const MULTIPLEXED: Partial<Record<HttpProtocol, Record<Direction, number>>> = {
  http2: { down: 1, up: 4 },
  http3: { down: 1, up: 1 },
};

type StreamPolicy = TestConfig["transferStreams"];

export interface PlanServer {
  server: { id: string; name: string };
  /** The discovery origin and the protocol its preflight came over, where upload control goes. */
  discovery: {
    origin: string;
    protocol: string | null;
    latency: ReadonlyMap<string, OriginTargets<Pick<LatencyTarget, "transport">>>;
  };
  throughput: {
    target: Pick<ThroughputTarget, "transport">;
    fetch: Pick<FetchTarget, "origin" | "protocol">;
  };
}

/** One direction's lanes: how many open, and how many must deliver before the stage is ready. */
export interface Lanes {
  planned: number;
  gating: number;
}
type StreamPlan = Map<string, Record<Direction, Lanes>>;

interface OriginBudget {
  /** One connection stays free for leases, checkpoints and sign-in checks; WebSockets hold none. */
  control: boolean;
  /** HTTP progress feeds, each holding a connection for the whole stage. */
  feeds: number;
  /** The lanes sharing what the reservations leave, in dealing order. */
  lanes: { name: string; lanes: Lanes }[];
}

export const normalizeStreamCount = (count: number) =>
  Number.isFinite(count) ? Math.min(MAX_STREAMS, Math.max(1, Math.round(count))) : 1;

/** The latency stage, or a transfer stage with loaded latency, runs a probe channel. */
export const needsPings = (activity: Activity) =>
  activity.stage === "latency" || (activity.transfer.length > 0 && activity.loadedLatency);

const controlMultiplexed = ({ protocol }: PlanServer["discovery"]) =>
  /^h[23]$/.test(protocol ?? "");

/** A fetch upload's feed reaches the discovery origin, where only proven HTTP/1.1 spares the pool by riding an offered
 *  WebSocket. */
export const feedSocket = (discovery: PlanServer["discovery"]): boolean =>
  discovery.protocol === "http/1.1" &&
  !!discovery.latency
    .get(discovery.origin)
    ?.targets.some(({ transport }) => transport === "websocket");

/** Every lane keeps one stream; the rest is dealt round-robin up to each ceiling. */
function deal(available: number, ceilings: readonly number[]): number[] {
  const dealt = ceilings.map(() => 0);
  for (let left = available, given = true; left > 0 && given; ) {
    given = false;
    for (let i = 0; i < ceilings.length && left > 0; i++)
      if (dealt[i]! < ceilings[i]!) {
        dealt[i] = dealt[i]! + 1;
        left--;
        given = true;
      }
  }
  return dealt;
}

/** Deals each HTTP/1.1 origin's pool among its lanes, refusing a stage that leaves one none; only lanes the pool runs gate readiness. */
export function planStreams(
  servers: readonly PlanServer[],
  activity: Activity,
  policy: StreamPolicy,
): StreamPlan {
  const count = normalizeStreamCount(policy.count);
  const forced = policy.mode === "forced";
  const plan: StreamPlan = new Map();
  const pools = new Map<string, OriginBudget>();
  const pool = (origin: string) => {
    let budget = pools.get(origin);
    if (!budget) pools.set(origin, (budget = { control: false, feeds: 0, lanes: [] }));
    return budget;
  };
  for (const { server, discovery, throughput } of servers) {
    const lanes: Record<Direction, Lanes> = {
      down: { planned: 0, gating: 0 },
      up: { planned: 0, gating: 0 },
    };
    plan.set(server.id, lanes);
    const session = throughput.target.transport !== "fetch-stream";
    const multiplexed = session ? undefined : MULTIPLEXED[throughput.fetch.protocol];
    const http1 = session || multiplexed ? null : pool(throughput.fetch.origin);
    for (const direction of activity.transfer) {
      const planned = session
        ? Math.min(SESSION_MAX_LANES, forced ? count : 1)
        : forced
          ? count
          : (multiplexed?.[direction] ?? count);
      lanes[direction] = { planned, gating: planned };
      http1?.lanes.push({ name: server.name, lanes: lanes[direction] });
    }
    const uploads = activity.transfer.includes("up");
    const control = pool(discovery.origin);
    if (needsPings(activity) || uploads) control.control = true;
    if (!session && uploads && !controlMultiplexed(discovery) && !feedSocket(discovery))
      control.feeds++;
  }
  for (const { control, feeds, lanes } of pools.values()) {
    if (!lanes.length) continue;
    const available = BROWSER_POOL - feeds - (control ? 1 : 0);
    if (available < lanes.length)
      throw new Error(
        `${[...new Set(lanes.map(({ name }) => name))].join(", ")} share one HTTP/1 origin without enough connections for this stage`,
      );
    const dealt = deal(
      available,
      lanes.map((each) => each.lanes.planned),
    );
    lanes.forEach((each, i) => {
      each.lanes.gating = dealt[i]!;
      if (!forced) each.lanes.planned = dealt[i]!;
    });
  }
  return plan;
}

export function planError(config: TestConfig, servers: readonly PlanServer[]): string {
  const planned = activities(config);
  if (!planned.length) return "Give at least one stage a duration";
  try {
    for (const activity of planned)
      if (activity.transfer.length) planStreams(servers, activity, config.transferStreams);
    return "";
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
}

// Before any path is verified, a description assumes HTTP/1.1 with a WebSocket on its origin.
const ASSUMED: PlanServer = {
  server: { id: "", name: "" },
  discovery: {
    origin: "",
    protocol: null,
    latency: new Map([["", { blocked: null, targets: [{ transport: "websocket" }] }]]),
  },
  throughput: {
    target: { transport: "fetch-stream" },
    fetch: { origin: "", protocol: "http1" },
  },
};

/** The lane policy in words for Settings, from the plan the run would make for `server` alone. */
export function describeStreams(
  policy: StreamPolicy,
  activities: readonly Activity[],
  server: PlanServer | null,
): { value: string; aside: string } {
  const count = normalizeStreamCount(policy.count);
  const transport = server?.throughput.target.transport;
  if (transport === "webtransport-datagram")
    return { value: "Datagram flood", aside: "no streams" };
  if (policy.mode === "forced")
    return {
      value: "Exact",
      aside:
        transport === "webtransport" && count > SESSION_MAX_LANES
          ? `${SESSION_MAX_LANES} per direction (capped from ${count} by the session)`
          : `${count} per direction`,
    };
  if (transport === "webtransport")
    return { value: "Automatic", aside: "1 continuous stream per direction" };
  const multiplexed = server && MULTIPLEXED[server.throughput.fetch.protocol];
  if (multiplexed)
    return { value: "Automatic", aside: `${multiplexed.down} download / ${multiplexed.up} upload` };
  const alone = server ?? ASSUMED;
  let most = 0;
  for (const activity of activities) {
    const lanes = planStreams([alone], activity, policy).get(alone.server.id)!;
    for (const direction of activity.transfer) most = Math.max(most, lanes[direction].planned);
  }
  return { value: "Automatic", aside: `up to ${most || count} per direction` };
}
