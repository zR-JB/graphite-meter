// Pure presentation behind EndpointInfo.svelte's diagnostics rows. The drawer
// itself reads the store, so anything with a branch in it lives here instead:
// a $derived.by in a component is reachable only from a browser.

import type { TransportDiscovery, TransportKind } from "../runner/contract";

/** Measurement occupancy as the server reported it at probe time. */
export interface ServerLoad {
  active: number;
  max: number;
}

/** Occupancy past this share means concurrent tests are contending for the
 *  bandwidth and CPU this run is measuring. */
const BUSY_SHARE = 0.5;

/** The occupancy row, or null when there is nothing to report. A server with no
 *  measurement slots configured has no occupancy: the share is not a number, so
 *  it can be neither idle nor busy, and the row would read zero of zero. */
export function serverLoadSummary(load: ServerLoad | undefined): string | null {
  if (!load || load.max <= 0) return null;
  const slots = `${load.active} of ${load.max} slots`;
  return load.active / load.max > BUSY_SHARE
    ? `${slots} · server busy — results may be affected`
    : slots;
}

/** Mechanisms advertised by the server and classified for this page. Runner
 * capabilities are intentionally excluded: they describe implementation, not
 * what this server offered to the current browser. */
export function advertisedServerCapabilities(
  discovery: TransportDiscovery | null,
  role: "throughput" | "latency",
): { transports: TransportKind[]; browserBlocked: boolean } | null {
  if (!discovery) return null;
  const transports = new Set<TransportKind>();
  let browserBlocked = false;
  for (const entry of Object.values(discovery[role])) {
    if (entry.state === "browser-blocked") browserBlocked = true;
    for (const target of entry.targets) transports.add(target.transport);
  }
  return { transports: [...transports], browserBlocked };
}
