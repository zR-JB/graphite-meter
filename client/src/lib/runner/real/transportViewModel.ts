import type { DiscoveredTarget, TransportDiscovery } from "../contract";
import type { FetchThroughputTarget, LatencyTarget } from "../../api/endpoints";
import {
  isLoopbackHostname,
  selectLatencyTarget,
  selectThroughputTarget,
} from "./backendPure";
import { describeTarget } from "./targetPresentation";

export interface TransportOptionView {
  disabled: boolean;
  detail: string;
}

const NOT_ADVERTISED = "Not offered in /preflight.";
const DISCOVERY_PENDING = "Checking server transports…";

function automaticDetail(
  target: FetchThroughputTarget | LatencyTarget,
  discovery: TransportDiscovery,
  role: "throughput" | "latency",
): string {
  const reason =
    target.origin === discovery.pageOrigin
      ? "matches this page"
      : `is the only available ${role} endpoint`;
  return `Selects ${target.origin} because it ${reason}.`;
}

function advertisedDetail(
  entry: DiscoveredTarget<FetchThroughputTarget | LatencyTarget>,
  discovery: TransportDiscovery,
): string {
  if (entry.state === "not-advertised") return NOT_ADVERTISED;
  if (entry.state === "browser-blocked")
    return `Blocked by the browser: a secure page cannot open this clear endpoint · ${entry.target?.origin ?? "unknown origin"}`;
  if (
    discovery.pageSecure &&
    entry.target &&
    !entry.target.tls &&
    isLoopbackHostname(new URL(entry.target.origin).hostname)
  )
    return `Browser-trusted clear loopback endpoint · ${entry.target.origin}`;
  return entry.target
    ? describeTarget(discovery, entry.target).advertisedDetail
    : NOT_ADVERTISED;
}

export function throughputOptionView(
  discovery: TransportDiscovery | null,
  selection: string,
): TransportOptionView {
  if (!discovery) return { disabled: true, detail: DISCOVERY_PENDING };
  if (selection === "current" || selection === "auto") {
    const target = selectThroughputTarget(discovery, selection);
    return target
      ? {
          disabled: false,
          detail: automaticDetail(target, discovery, "throughput"),
        }
      : {
          disabled: true,
          detail: "No offered target matches this page origin and protocol.",
        };
  }
  const entry = discovery.throughput[selection];
  return {
    disabled: entry?.state !== "advertised",
    detail: entry ? advertisedDetail(entry, discovery) : NOT_ADVERTISED,
  };
}

export function latencyOptionView(
  discovery: TransportDiscovery | null,
  selection: string,
): TransportOptionView {
  if (!discovery) return { disabled: true, detail: DISCOVERY_PENDING };
  if (selection === "auto") {
    const target = selectLatencyTarget(discovery, selection);
    return target
      ? {
          disabled: false,
          detail: automaticDetail(target, discovery, "latency"),
        }
      : {
          disabled: true,
          detail: `${discovery.pageSecure ? "Secure" : "Clear"} WebSocket target is not offered in /preflight.`,
        };
  }
  const entry = discovery.latency[selection];
  return {
    disabled: entry?.state !== "advertised",
    detail: entry ? advertisedDetail(entry, discovery) : NOT_ADVERTISED,
  };
}
