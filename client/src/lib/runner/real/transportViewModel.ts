import type { DiscoveredTarget, TransportDiscovery } from "../contract";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../../api/endpoints";
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

function automaticDetail(
  target: FetchThroughputTarget | WebSocketLatencyTarget,
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
  entry: DiscoveredTarget<FetchThroughputTarget | WebSocketLatencyTarget>,
  discovery: TransportDiscovery,
): string {
  if (entry.state === "not-advertised") return "Not offered in /preflight.";
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
    : "Not offered in /preflight.";
}

export function throughputOptionView(
  discovery: TransportDiscovery | null,
  selection: string,
): TransportOptionView {
  if (!discovery)
    return { disabled: true, detail: "Checking server transports…" };
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
    detail: entry
      ? advertisedDetail(entry, discovery)
      : "Not offered in /preflight.",
  };
}

export function latencyOptionView(
  discovery: TransportDiscovery | null,
  selection: string,
): TransportOptionView {
  if (!discovery)
    return { disabled: true, detail: "Checking server transports…" };
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
    detail: entry
      ? advertisedDetail(entry, discovery)
      : "Not offered in /preflight.",
  };
}
