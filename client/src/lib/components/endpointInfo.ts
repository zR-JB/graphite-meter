// Pure presentation behind EndpointInfo.svelte's diagnostics rows.

import type {
  DiscoveredLatency,
  DiscoveredThroughput,
  TransportDiscovery,
  TransportKind,
} from "../runner/contract";
import { httpProtocolLabel } from "../runner/protocol";
import type { ConnectionValidationState } from "../runner/connectionModel";

type EndpointPathMode = "live" | "running" | "result";

export function endpointPathStatus(
  validation: ConnectionValidationState,
  mode: EndpointPathMode,
): {
  label: string;
  tone: "ready" | "active" | "used" | ConnectionValidationState;
} {
  if (validation !== "verified")
    return {
      label: validation[0].toUpperCase() + validation.slice(1),
      tone: validation,
    };
  const verified = {
    live: { label: "Ready", tone: "ready" },
    running: { label: "In use", tone: "active" },
    result: { label: "Used", tone: "used" },
  } as const;
  return verified[mode];
}

/** Measurement occupancy as the server reported it at probe time. */
interface ServerLoad {
  active: number;
  max: number;
}

/** Occupancy past this share means concurrent tests are contending for the bandwidth and CPU this run is measuring. */
const BUSY_SHARE = 0.5;

/* A server with no measurement slots configured has no occupancy: the share is not a number, so it can be neither. */
export function serverLoadSummary(load: ServerLoad | undefined): string | null {
  if (!load || load.max <= 0) return null;
  const slots = `${load.active} of ${load.max} slots`;
  return load.active / load.max > BUSY_SHARE
    ? `${slots} · server busy — results may be affected`
    : slots;
}

/* Mechanisms advertised by the server and classified for this page. */
export function advertisedServerCapabilities(
  discovery: TransportDiscovery | null,
  role: "throughput" | "latency",
): { transports: TransportKind[]; browserBlocked: boolean } | null {
  if (!discovery) return null;
  const entries = Object.values(discovery[role]) as (
    DiscoveredThroughput | DiscoveredLatency
  )[];
  return {
    transports: [
      ...new Set(
        entries.flatMap((entry) =>
          entry.targets.map((target) => target.transport),
        ),
      ),
    ],
    browserBlocked: entries.some((entry) => entry.state === "browser-blocked"),
  };
}

/* Fetch paths are the HTTP capability contract; other transports belong to their path cards. */
export function advertisedServerHttpPaths(
  discovery: TransportDiscovery | null,
): string[] | null {
  if (!discovery) return null;
  const targets = [
    ...Object.values(discovery.throughput)
      .flatMap((entry) => entry.targets)
      .filter((target) => target.transport === "fetch-stream"),
  ];
  return [
    ...new Set(
      targets.map(
        (target) =>
          `${httpProtocolLabel(target.protocol)} · ${target.tls ? "TLS" : "clear"}`,
      ),
    ),
  ];
}

/* Protocol evidence has distinct observation points. */
export function pathEvidence(
  role: "throughput" | "latency",
  browserProtocol?: string,
  serverProtocol?: string,
): string {
  const evidence = [
    role === "throughput" && browserProtocol
      ? `Browser observed ${httpProtocolLabel(browserProtocol)}`
      : null,
    serverProtocol
      ? `Server observed ${httpProtocolLabel(serverProtocol)}`
      : null,
  ].filter((value): value is string => value != null);
  return evidence.join(" · ") || "Pending";
}
