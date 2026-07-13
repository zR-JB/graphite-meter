/* ============================================================
 * RealBackend pure helpers — origin/URL mapping, small math, and stage-
 * activity queries with no fetch/worker/websocket entanglement. Split out
 * of RealRunner.ts so they're unit-testable without pulling in its build-time
 * BUILD defines.
 * ============================================================ */

import type {
  RunnerConfig,
  PhaseActivity,
  ProtocolTarget,
  TransferTargetSelection,
} from "../contract";
import type { ChannelTarget, TransferTarget } from "../../api/preflight";

const protocolByNextHop: Partial<Record<string, ProtocolTarget>> = {
  "http/1.1": "http1",
  h2: "http2",
  h3: "http3",
};

/** Resolve one bulk transfer path. Target ids distinguish clear and TLS H1;
 *  protocol evidence disambiguates multiple targets sharing an origin. */
export function selectTransferTarget(
  targets: TransferTarget[],
  selection: TransferTargetSelection,
  discoveryOrigin: string,
  securePage: boolean,
  discoveryProtocol?: string,
): TransferTarget | null {
  const usable = targets.filter((target) => !(securePage && !target.tls));
  if (selection !== "current")
    return usable.find((target) => target.id === selection) ?? null;
  const current = usable.filter((target) => target.origin === discoveryOrigin);
  const observed = discoveryProtocol
    ? protocolByNextHop[discoveryProtocol]
    : undefined;
  return (
    current.find((target) => target.protocol === observed) ??
    (current.length === 1 ? current[0] : null) ??
    usable.find(
      (target) => target.id === (securePage ? "http1-tls" : "http1-clear"),
    ) ??
    usable[0] ??
    null
  );
}

export function browserProtocolMatchesTarget(
  target: TransferTarget,
  nextHopProtocol?: string,
): boolean {
  return (
    !!nextHopProtocol && protocolByNextHop[nextHopProtocol] === target.protocol
  );
}

export function transferTargetKey(target: TransferTarget | null): string {
  return target ? `${target.id}\n${target.origin}` : "";
}

export type ChannelRole = "latency" | "uploadProgress";

/** Bind a message role independently from throughput. Automatic binding keeps
 *  the selected transfer origin when possible, but can use any advertised
 *  channel. Explicit ids make future cross-transport combinations stable. */
export function selectChannelTarget(
  channels: ChannelTarget[],
  role: ChannelRole,
  selection: "auto" | string,
  transfer: TransferTarget,
  securePage: boolean,
  runnable: ReadonlySet<ChannelTarget["transport"]>,
): ChannelTarget | null {
  const usable = channels.filter(
    (channel) =>
      channel.routes[role] !== null &&
      runnable.has(channel.transport) &&
      !(securePage && !channel.tls),
  );
  if (selection !== "auto")
    return usable.find((channel) => channel.id === selection) ?? null;
  return (
    usable.find((channel) => channel.origin === transfer.origin) ??
    usable[0] ??
    null
  );
}

/** Resolve the fetch base URL for the backend. `host:"auto"` (or empty) means
 *  same-origin (relative requests) — the Stage-1 case where the Go server serves
 *  both the app and the API. A concrete host builds an absolute origin. */
export function resolveBase(endpoint?: RunnerConfig["endpoint"]): string {
  if (!endpoint || endpoint.host === "auto" || endpoint.host === "") return "";
  const scheme = endpoint.port === 443 ? "https" : "http";
  return `${scheme}://${endpoint.host}:${endpoint.port}`;
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

/** Upgrade a ws:// base to wss://, unchanged otherwise. Used to force the
 *  latency bus encrypted when the page itself loaded over https, regardless
 *  of what scheme the server-advertised origin guessed at. */
export function wsToWss(base: string): string {
  return base.startsWith("ws://")
    ? "wss://" + base.slice("ws://".length)
    : base;
}

/** Median of a non-empty number list (used for the pre-test ping). */
export function median(xs: number[]): number {
  const sorted = [...xs].sort((a, b) => a - b);
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

/** Per-lane spawn delay for `streams` parallel lanes over a `warmupMs`
 *  warmup window, capped at `baseMs` (RealRunner's LANE_STAGGER_MS) but
 *  shrunk so even the last lane (index streams-1) still starts within half
 *  the warmup. Zero (spawn together) for a single lane or no warmup. */
export function laneStaggerMs(
  streams: number,
  warmupMs: number,
  baseMs: number,
): number {
  return streams > 1
    ? Math.min(baseMs, Math.floor((warmupMs * 0.5) / (streams - 1)))
    : 0;
}
