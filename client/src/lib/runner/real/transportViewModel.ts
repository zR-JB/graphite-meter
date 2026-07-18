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

export interface TransportOptionView {
  disabled: boolean;
  detail: string;
}

function advertisedDetail(
  entry: DiscoveredTarget<FetchThroughputTarget | WebSocketLatencyTarget>,
  discovery: TransportDiscovery,
): string {
  if (entry.state === "not-advertised") return "Not offered in /preflight.";
  if (entry.state === "browser-blocked")
    return "Advertised, but blocked from this secure page. Open the Graphite Meter UI over HTTP to test this clear target.";
  if (
    discovery.pageSecure &&
    entry.target &&
    !entry.target.tls &&
    isLoopbackHostname(new URL(entry.target.origin).hostname)
  )
    return "Allowed as a browser-trusted loopback target.";
  return "Offered by server.";
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
          detail: "Uses this page's connection.",
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
          detail: "Uses the same security as this page.",
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
