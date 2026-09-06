import type {
  FlowDirection,
  PhaseActivity,
  PreparedPaths,
  RunnerConfig,
} from "../runner/contract";
import {
  BROWSER_CONNECTION_BUDGET,
  transferStreamCount,
} from "../runner/real/streamPolicy";
import { needsPings } from "../runner/real/backendPure";

export type ServerStreamPlan = Record<string, Record<FlowDirection, number>>;
/** Browser pools may be shared by catalogue entries. Leave a control slot for fresh checkpoints. */
export function planServerStreams(
  config: RunnerConfig,
  paths: readonly { id: string; paths: PreparedPaths }[],
  activity: PhaseActivity,
): ServerStreamPlan {
  const plan: ServerStreamPlan = Object.create(null);
  const h1 = new Map<string, { id: string; direction: FlowDirection }[]>();
  const control = new Map<string, number>();
  for (const server of paths) {
    const throughput = server.paths.throughput;
    const wt = throughput.target.transport !== "fetch-stream";
    plan[server.id] = { down: 0, up: 0 };
    for (const dir of activity.transfer) {
      plan[server.id][dir] = transferStreamCount({
        protocol: throughput.fetch.protocol,
        policy: config.transferStreams,
        transfer: activity.transfer,
        dir,
        needsPing: needsPings(activity) && server.paths.latency !== null,
        webTransport: wt,
      });
      if (!wt && !["http2", "http3"].includes(throughput.fetch.protocol)) {
        const group = h1.get(throughput.fetch.origin) ?? [];
        group.push({ id: server.id, direction: dir });
        h1.set(throughput.fetch.origin, group);
      }
    }
    if (activity.transfer.includes("up") && !wt)
      control.set(
        throughput.fetch.origin,
        (control.get(throughput.fetch.origin) ?? 0) + 1,
      );
    if (
      needsPings(activity) &&
      server.paths.latency?.target.transport === "websocket"
    ) {
      const origin = server.paths.latency.target.origin;
      control.set(origin, (control.get(origin) ?? 0) + 1);
    }
  }
  for (const [origin, lanes] of h1) {
    const available =
      BROWSER_CONNECTION_BUDGET -
      (control.get(origin) ?? 0) -
      (activity.transfer.includes("up") ? 1 : 0);
    if (available < lanes.length)
      throw new Error(
        `The selected servers share ${origin}, which has insufficient HTTP/1 connection capacity for this stage`,
      );
    const wanted = lanes.reduce(
      (total, lane) => total + plan[lane.id][lane.direction],
      0,
    );
    if (wanted <= available) continue;
    if (config.transferStreams.mode === "forced")
      throw new Error(
        `Forced streams would occupy the progress and control capacity at ${origin}. Reduce streams or use Automatic`,
      );
    let remaining = available - lanes.length;
    const ceilings = lanes.map((lane) => plan[lane.id][lane.direction]);
    lanes.forEach((lane) => (plan[lane.id][lane.direction] = 1));
    while (remaining > 0) {
      let assigned = false;
      for (const [index, lane] of lanes.entries())
        if (remaining && plan[lane.id][lane.direction] < ceilings[index]) {
          plan[lane.id][lane.direction]++;
          remaining--;
          assigned = true;
        }
      if (!assigned) break;
    }
  }
  for (const dir of activity.transfer)
    if (
      Object.values(plan).reduce((total, count) => total + count[dir], 0) > 128
    )
      throw new Error(
        "The run exceeds 128 streams per direction. Reduce forced streams",
      );
  return plan;
}
