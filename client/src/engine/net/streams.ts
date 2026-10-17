// Plans one stage's lanes per server and direction from one model of each HTTP/1.1 origin's browser connection pool.
// Control requests reserve connections by name, lanes share the rest, and the lanes the pool can run are the lanes
// that must deliver before the stage is ready.
import { activities, type Activity } from "../run/schedule";
import type { Direction, FetchTarget, HttpProtocol, LatencyTarget, TestConfig, ThroughputTarget } from "../types";

/** A browser runs this many concurrent HTTP/1.1 requests per origin. More wait for a free connection. */
export const BROWSER_POOL = 6;
/** The server clamps a WebTransport session's lanes here in both directions. */
const SESSION_MAX_LANES = 16;
export const MAX_STREAMS = 128;
// Upload splits by protocol, since under loss HTTP/2 gains from four lanes while HTTP/3 loses.
const MULTIPLEXED: Partial<Record<HttpProtocol, Record<Direction, number>>> = {
  http2: { down: 1, up: 4 },
  http3: { down: 1, up: 1 },
};

type StreamPolicy = TestConfig["transferStreams"];

/** Planning reads this much of a server's verified paths. */
export interface PlanServer {
  server: { id: string; name: string };
  throughput: { target: Pick<ThroughputTarget, "transport">; fetch: Pick<FetchTarget, "origin" | "protocol"> };
  latency: { target: Pick<LatencyTarget, "transport" | "origin"> } | null;
}

/** One direction's lanes say how many open, and how many must deliver before the stage is ready. */
export interface Lanes {
  planned: number;
  gating: number;
}
type StreamPlan = Map<string, Record<Direction, Lanes>>;

interface OriginBudget {
  /** What holds one of the origin's connections for the whole stage. */
  reserved: ("pings" | "feed" | "checkpoint")[];
  /** These lanes share what the reservations leave, in dealing order. */
  lanes: { name: string; lanes: Lanes }[];
}

export const normalizeStreamCount = (count: number) =>
  Number.isFinite(count) ? Math.min(MAX_STREAMS, Math.max(1, Math.round(count))) : 1;

/** The latency stage, or a transfer stage with loaded latency, runs a probe channel. */
export const needsPings = (activity: Activity) =>
  activity.stage === "latency" || (activity.transfer.length > 0 && activity.loadedLatency);

/** Every lane keeps one stream. The rest is dealt round-robin up to each ceiling. */
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

/** Plans lanes per server and direction for one stage. Each HTTP/1.1 origin's pool (unproven negotiated HTTP counts
    as HTTP/1.1) is dealt among its lanes, refusing a stage where some lane would get no connection. Automatic opens
    what is dealt, forced counts open exactly and only the lanes their pool can run gate readiness. */
export function planStreams(servers: readonly PlanServer[], activity: Activity, policy: StreamPolicy): StreamPlan {
  const count = normalizeStreamCount(policy.count);
  const forced = policy.mode === "forced";
  const plan: StreamPlan = new Map();
  const pools = new Map<string, OriginBudget>();
  const pool = (origin: string) => {
    let budget = pools.get(origin);
    if (!budget) pools.set(origin, (budget = { reserved: [], lanes: [] }));
    return budget;
  };
  for (const { server, throughput, latency } of servers) {
    const lanes: Record<Direction, Lanes> = { down: { planned: 0, gating: 0 }, up: { planned: 0, gating: 0 } };
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
    if (needsPings(activity) && latency?.target.transport === "websocket")
      pool(latency.target.origin).reserved.push("pings");
    if (!activity.transfer.includes("up")) continue;
    // Receiver checkpoints reach the HTTP view of every upload; one connection serves all of an origin's.
    const control = pool(throughput.fetch.origin);
    if (!session) control.reserved.push("feed");
    if (!control.reserved.includes("checkpoint")) control.reserved.push("checkpoint");
  }
  for (const { reserved, lanes } of pools.values()) {
    if (!lanes.length) continue;
    const available = BROWSER_POOL - reserved.length;
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

/** Says why `config`'s stages cannot run on `servers`, or "" when they can. */
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

// Before any path is verified, a description assumes HTTP/1.1 with the WebSocket latency bus on the same origin.
const ASSUMED: PlanServer = {
  server: { id: "", name: "" },
  throughput: { target: { transport: "fetch-stream" }, fetch: { origin: "", protocol: "negotiated" } },
  latency: { target: { transport: "websocket", origin: "" } },
};

/** Words the lane policy for Settings as a value and its qualifier, read from the plan the run would make for
    `server` on its own over `activities`. */
export function describeStreams(
  policy: StreamPolicy,
  activities: readonly Activity[],
  server: PlanServer | null,
): { value: string; aside: string } {
  const count = normalizeStreamCount(policy.count);
  const transport = server?.throughput.target.transport;
  if (transport === "webtransport-datagram") return { value: "Datagram flood", aside: "no streams" };
  if (policy.mode === "forced")
    return {
      value: "Exact",
      aside:
        transport === "webtransport" && count > SESSION_MAX_LANES
          ? `${SESSION_MAX_LANES} per direction (capped from ${count} by the session)`
          : `${count} per direction`,
    };
  if (transport === "webtransport") return { value: "Automatic", aside: "1 continuous stream per direction" };
  const multiplexed = server && MULTIPLEXED[server.throughput.fetch.protocol];
  if (multiplexed) return { value: "Automatic", aside: `${multiplexed.down} download / ${multiplexed.up} upload` };
  const alone = server ?? ASSUMED;
  let most = 0;
  for (const activity of activities) {
    const lanes = planStreams([alone], activity, policy).get(alone.server.id)!;
    for (const direction of activity.transfer) most = Math.max(most, lanes[direction].planned);
  }
  return { value: "Automatic", aside: `up to ${most || count} per direction` };
}
