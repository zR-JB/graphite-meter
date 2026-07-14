/* ============================================================
 * RealBackend pure helpers — origin/URL mapping, small math, and stage-
 * activity queries with no fetch/worker/websocket entanglement. Split out
 * of RealRunner.ts so they're unit-testable without pulling in its build-time
 * BUILD defines.
 * ============================================================ */

import type {
  PhaseActivity,
  ProtocolTarget,
  TransportDiscovery,
  ThroughputTargetSelection,
} from "../contract";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  ThroughputTarget,
  WebSocketLatencyTarget,
} from "../../api/preflight";

const protocolByNextHop: Partial<Record<string, ProtocolTarget>> = {
  "http/1.1": "http1",
  h2: "http2",
  h3: "http3",
};

const THROUGHPUT_IDS = ["http1-clear", "http1-tls", "http2", "http3"];
const LATENCY_IDS = ["ws-http1-clear", "ws-http1-tls"];

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

function usableFromPage(origin: string, tls: boolean, pageSecure: boolean) {
  if (!pageSecure || tls) return true;
  try {
    return isLoopbackHostname(new URL(origin).hostname);
  } catch {
    return false;
  }
}

/** Classify the logical server catalog once for both selection and settings. */
export function classifyTransportDiscovery(
  throughputTargets: ThroughputTarget[],
  latencyTargets: LatencyTarget[],
  pageOrigin: string,
  pageSecure: boolean,
  pageProtocol?: string,
): TransportDiscovery {
  const throughput = Object.fromEntries(
    THROUGHPUT_IDS.map((id) => {
      const target = throughputTargets.find(
        (candidate): candidate is FetchThroughputTarget =>
          candidate.id === id && candidate.transport === "fetch-stream",
      );
      return [
        id,
        !target
          ? { state: "not-advertised" as const }
          : {
              state: usableFromPage(target.origin, target.tls, pageSecure)
                ? ("advertised" as const)
                : ("browser-blocked" as const),
              target,
            },
      ];
    }),
  );
  const latency = Object.fromEntries(
    LATENCY_IDS.map((id) => {
      const target = latencyTargets.find(
        (candidate): candidate is WebSocketLatencyTarget =>
          candidate.id === id && candidate.transport === "websocket",
      );
      return [
        id,
        !target
          ? { state: "not-advertised" as const }
          : {
              state: usableFromPage(target.origin, target.tls, pageSecure)
                ? ("advertised" as const)
                : ("browser-blocked" as const),
              target,
            },
      ];
    }),
  );
  return { pageOrigin, pageSecure, pageProtocol, throughput, latency };
}

/** Resolve one bulk transfer path. Target ids distinguish clear and TLS H1;
 *  protocol evidence disambiguates multiple targets sharing an origin. */
export function selectThroughputTarget(
  discovery: TransportDiscovery,
  selection: ThroughputTargetSelection,
): FetchThroughputTarget | null {
  if (selection !== "current") {
    const entry = discovery.throughput[selection];
    return entry?.state === "advertised" ? (entry.target ?? null) : null;
  }
  const observed = discovery.pageProtocol
    ? protocolByNextHop[discovery.pageProtocol]
    : undefined;
  if (!observed) return null;
  return (
    Object.values(discovery.throughput).find(
      (entry) =>
        entry.state === "advertised" &&
        entry.target?.origin === discovery.pageOrigin &&
        entry.target.protocol === observed,
    )?.target ?? null
  );
}

export function browserProtocolMatchesTarget(
  target: FetchThroughputTarget,
  nextHopProtocol?: string,
): boolean {
  return (
    !!nextHopProtocol && protocolByNextHop[nextHopProtocol] === target.protocol
  );
}

export function throughputTargetKey(target: ThroughputTarget | null): string {
  return target ? `${target.id}\n${target.origin}` : "";
}

/** Select latency independently. Auto follows page security, not throughput. */
export function selectLatencyTarget(
  discovery: TransportDiscovery,
  selection: "auto" | string,
): WebSocketLatencyTarget | null {
  const id =
    selection === "auto"
      ? discovery.pageSecure
        ? "ws-http1-tls"
        : "ws-http1-clear"
      : selection;
  const entry = discovery.latency[id];
  return entry?.state === "advertised" ? (entry.target ?? null) : null;
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
