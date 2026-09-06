import type { DiscoveredTarget, TransportDiscovery } from "../contract";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../../api/endpoints";
import {
  isLoopbackHostname,
  blockedSelectionReason,
  locateTarget,
  selectLatencyTarget,
  selectThroughputTarget,
} from "./backendPure";
import { describeTarget } from "./targetPresentation";
import { webTransportGap } from "./transports";

interface TransportOptionView {
  disabled: boolean;
  detail: string;
}

const NOT_ADVERTISED = "Not offered in /preflight.";
const DISCOVERY_PENDING = "Checking server transports…";

/* Why an advertised card cannot be driven here. */
function noBrowserWebTransport(): string {
  return webTransportGap() === "insecure-page"
    ? "Needs a secure page: browsers offer WebTransport over HTTPS only — reopen this page on its https:// address."
    : "WebTransport is unavailable in this browser.";
}

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

/* Describes an origin through whichever mechanism the selection named, or its first when the state rules the. */
function advertisedDetail(
  entry: DiscoveredTarget<
    FetchThroughputTarget | WebTransportThroughputTarget | LatencyTarget
  >,
  discovery: TransportDiscovery,
  named = entry.targets[0],
): string {
  if (entry.state === "not-advertised" || !named) return NOT_ADVERTISED;
  if (entry.state === "browser-blocked")
    return (
      entry.blockedReason ??
      `Blocked by the browser: a secure page cannot open this clear endpoint · ${named.origin}`
    );
  if (
    discovery.pageSecure &&
    !named.tls &&
    isLoopbackHostname(new URL(named.origin).hostname)
  )
    return `Browser-trusted clear loopback endpoint · ${named.origin}`;
  return describeTarget(discovery, named).advertisedDetail;
}

/** A mechanism this browser has no API for is offered by the server but cannot be driven here. */
function needsMissingWebTransport(kind: string): boolean {
  return (
    (kind === "webtransport" || kind === "webtransport-datagram") &&
    typeof WebTransport === "undefined"
  );
}

export function throughputOptionView(
  discovery: TransportDiscovery | null,
  selection: string,
): TransportOptionView {
  if (!discovery) return { disabled: true, detail: DISCOVERY_PENDING };
  const restriction = blockedSelectionReason(
    discovery,
    "throughput",
    selection,
  );
  if (restriction) return { disabled: true, detail: restriction };
  if (selection === "current" || selection === "auto") {
    // Resolve exactly what the runner resolves, so the automatic card never offers the session path a.
    const runnable = typeof WebTransport !== "undefined";
    const target = selectThroughputTarget(discovery, selection, runnable);
    if (target)
      return {
        disabled: false,
        detail: automaticDetail(target, discovery, "throughput"),
      };
    // Automatic's last resort is a session origin: say which of the two reasons left the card unresolved rather than.
    const refused =
      !runnable && selectThroughputTarget(discovery, selection, true);
    return {
      disabled: true,
      detail: refused
        ? noBrowserWebTransport()
        : "No offered target matches this page origin and protocol.",
    };
  }
  const found = locateTarget(discovery.throughput, selection);
  if (found && needsMissingWebTransport(found.target.transport))
    return { disabled: true, detail: noBrowserWebTransport() };
  const entry = found?.entry ?? discovery.throughput[selection];
  return {
    disabled: entry?.state !== "advertised" || !found,
    detail: entry
      ? advertisedDetail(entry, discovery, found?.target)
      : NOT_ADVERTISED,
  };
}

export function latencyOptionView(
  discovery: TransportDiscovery | null,
  selection: string,
): TransportOptionView {
  if (!discovery) return { disabled: true, detail: DISCOVERY_PENDING };
  const restriction = blockedSelectionReason(discovery, "latency", selection);
  if (restriction) return { disabled: true, detail: restriction };
  // Resolve exactly what the runner resolves, so a card never offers a bus the run would refuse.
  const runnable = typeof WebTransport !== "undefined";
  if (selection === "auto") {
    const target = selectLatencyTarget(discovery, selection, runnable);
    if (target)
      return {
        disabled: false,
        detail: automaticDetail(target, discovery, "latency"),
      };
    // An h3-only deployment advertises a datagram bus and no WebSocket, so blaming the server here tells a browser.
    const refused =
      !runnable && selectLatencyTarget(discovery, selection, true);
    return {
      disabled: true,
      detail: refused
        ? noBrowserWebTransport()
        : `${discovery.pageSecure ? "Secure" : "Clear"} WebSocket target is not offered in /preflight.`,
    };
  }
  const target = selectLatencyTarget(discovery, selection, runnable);
  if (target)
    return {
      disabled: false,
      detail: describeTarget(discovery, target).advertisedDetail,
    };
  const found = locateTarget(discovery.latency, selection);
  const entry = found?.entry ?? discovery.latency[selection];
  // A bus the server offers but this browser cannot drive says so, rather than reading as something the server failed.
  const blocked = found
    ? needsMissingWebTransport(found.target.transport)
    : !runnable && entry?.targets.every((t) => t.transport === "webtransport");
  if (blocked) return { disabled: true, detail: noBrowserWebTransport() };
  return {
    disabled: true,
    detail: entry
      ? advertisedDetail(entry, discovery, found?.target)
      : NOT_ADVERTISED,
  };
}
