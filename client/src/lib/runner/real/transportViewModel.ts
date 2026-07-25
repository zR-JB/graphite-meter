import type { DiscoveredTarget, TransportDiscovery } from "../contract";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../../api/endpoints";
import {
  isLoopbackHostname,
  selectLatencyTarget,
  selectThroughputTarget,
  WT_SELECTION_SUFFIX,
} from "./backendPure";
import { describeTarget } from "./targetPresentation";

export interface TransportOptionView {
  disabled: boolean;
  detail: string;
}

const NOT_ADVERTISED = "Not offered in /preflight.";
const DISCOVERY_PENDING = "Checking server transports…";
const NO_BROWSER_WT = "WebTransport is not supported by this browser.";

function automaticDetail(
  target: FetchThroughputTarget | WebTransportThroughputTarget | LatencyTarget,
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
  if (selection.endsWith(WT_SELECTION_SUFFIX)) {
    if (typeof WebTransport === "undefined")
      return { disabled: true, detail: NO_BROWSER_WT };
    const entry =
      discovery.throughput[selection.slice(0, -WT_SELECTION_SUFFIX.length)];
    return {
      disabled: entry?.state !== "advertised" || !entry.wt,
      detail: entry?.wt
        ? describeTarget(discovery, entry.wt).advertisedDetail
        : NOT_ADVERTISED,
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
    const target = selectLatencyTarget(
      discovery,
      selection,
      typeof WebTransport !== "undefined",
    );
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
  if (
    entry?.target?.transport === "webtransport" &&
    typeof WebTransport === "undefined"
  )
    return { disabled: true, detail: NO_BROWSER_WT };
  return {
    disabled: entry?.state !== "advertised",
    detail: entry ? advertisedDetail(entry, discovery) : NOT_ADVERTISED,
  };
}
