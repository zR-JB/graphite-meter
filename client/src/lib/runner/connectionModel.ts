import type {
  ConnectionRole,
  DiscoveredTarget,
  PreparedPaths,
  ProtocolTarget,
  RunnerConfig,
  RunnerError,
  TransportDiscovery,
  VerifiedLatencyPath,
  VerifiedThroughputPath,
} from "./contract";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../api/endpoints";
import {
  locateTarget,
  selectLatencyTarget,
  selectThroughputTarget,
} from "./real/backendPure";
import { describeTarget } from "./real/targetPresentation";

export type ConnectionValidationState =
  "checking" | "verified" | "failed" | "stale";
export type { ConnectionRole } from "./contract";
export interface RoleValidation<Path> {
  selection: string;
  state: ConnectionValidationState;
  path: Path | null;
  message?: string;
}
export interface ConnectionValidation {
  throughput: RoleValidation<VerifiedThroughputPath>;
  latency: RoleValidation<VerifiedLatencyPath>;
}
export const emptyConnectionValidation = (): ConnectionValidation => ({
  throughput: { selection: "current", state: "stale", path: null },
  latency: { selection: "auto", state: "stale", path: null },
});

export interface ConnectionPresentation {
  role: ConnectionRole;
  selection: string;
  target:
    FetchThroughputTarget | WebTransportThroughputTarget | LatencyTarget | null;
  availability: "advertised" | "browser-blocked" | "not-advertised";
  validation: ConnectionValidationState;
  label: string;
  summary: string;
  message?: string;
  observedProtocol?: ProtocolTarget;
  browserProtocol?: string;
  serverProtocol?: string;
  clientIp?: string;
  clientIpVersion?: 4 | 6;
  clientIpSource?: "socket" | "forwarded";
  preTestPingMs?: number;
  verifiedAt?: number;
}

export const CONNECTION_FRESH_MS = 2 * 60_000;
export const CONNECTION_FAILURE_BACKOFF_MS = [
  30_000, 60_000, 120_000, 240_000, 300_000,
] as const;
export const CONNECTION_ROLES: ConnectionRole[] = ["throughput", "latency"];
export function connectionFailureBackoff(attempt: number): number {
  return CONNECTION_FAILURE_BACKOFF_MS[
    Math.max(0, Math.min(attempt - 1, CONNECTION_FAILURE_BACKOFF_MS.length - 1))
  ];
}
export const CONNECTION_FAILURE_REASONS = new Set<RunnerError["reason"]>([
  "connection-lost",
  "timeout",
  "preflight-failed",
  "transport-unavailable",
]);
export function connectionSelection(
  config: RunnerConfig,
  role: ConnectionRole,
): string {
  return role === "throughput"
    ? config.transports.throughputTarget
    : config.transports.latencyTarget;
}
export function latencyPathNeeded(config: RunnerConfig): boolean {
  return (
    config.stages.latency ||
    (!config.skipLoadedLatencyWhenStageOff &&
      (config.stages.download ||
        config.stages.upload ||
        config.stages.bidirectional))
  );
}
export function selectTarget(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  selection: string,
) {
  return role === "throughput"
    ? selectThroughputTarget(
        discovery,
        selection,
        typeof WebTransport !== "undefined",
      )
    : selectLatencyTarget(
        discovery,
        selection,
        typeof WebTransport !== "undefined",
      );
}
export function connectionDraftRoleKey(
  config: RunnerConfig,
  role: ConnectionRole,
): string {
  const selection = connectionSelection(config, role);
  return role === "throughput"
    ? selection
    : JSON.stringify({ selection, needed: latencyPathNeeded(config) });
}
export function connectionDraftKey(config: RunnerConfig): string {
  return JSON.stringify(
    CONNECTION_ROLES.map((role) => connectionDraftRoleKey(config, role)),
  );
}
export function roleNeedsValidation(
  config: RunnerConfig,
  validation: ConnectionValidation,
  role: ConnectionRole,
  discovery?: TransportDiscovery | null,
): boolean {
  if (role === "latency" && !latencyPathNeeded(config)) return false;
  const check = validation[role];
  return (
    check.state !== "verified" ||
    !check.path ||
    !discovery ||
    check.path.generation !== discovery.generation ||
    JSON.stringify(check.path.requested) !==
      JSON.stringify(
        selectTarget(discovery, role, connectionSelection(config, role)),
      )
  );
}
export function validationRoles(
  config: RunnerConfig,
  validation: ConnectionValidation,
  requestedRole?: ConnectionRole,
  discovery?: TransportDiscovery | null,
): ConnectionRole[] {
  return CONNECTION_ROLES.filter((role) => {
    if (!requestedRole || role === requestedRole) return true;
    if (role === "latency" && !latencyPathNeeded(config)) return false;
    const check = validation[role];
    return (
      check.state === "stale" ||
      (check.path && discovery
        ? JSON.stringify(check.path.requested) !==
          JSON.stringify(
            selectTarget(discovery, role, connectionSelection(config, role)),
          )
        : check.selection !== connectionSelection(config, role))
    );
  });
}

/** There is no second prepared cache: freshness belongs to each verified role. */
export function preparedPaths(
  config: RunnerConfig,
  discovery: TransportDiscovery | null,
  validation: ConnectionValidation,
): PreparedPaths | null {
  if (
    !discovery ||
    CONNECTION_ROLES.some(
      (role) =>
        roleNeedsValidation(config, validation, role, discovery) ||
        ((role === "throughput" || latencyPathNeeded(config)) &&
          Date.now() - validation[role].path!.verifiedAt > CONNECTION_FRESH_MS),
    )
  )
    return null;
  return {
    discovery,
    throughput: validation.throughput.path!,
    latency: latencyPathNeeded(config) ? validation.latency.path : null,
  };
}

function availability(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  selection: string,
): ConnectionPresentation["availability"] {
  if (selection === "current" || selection === "auto")
    return selectTarget(discovery, role, selection)
      ? "advertised"
      : "not-advertised";
  const byOrigin: Record<string, DiscoveredTarget<{ id: string }>> = discovery[
    role
  ];
  return (
    locateTarget(byOrigin, selection)?.entry.state ??
    byOrigin[selection]?.state ??
    "not-advertised"
  );
}

export function presentConnections(
  config: RunnerConfig,
  discovery: TransportDiscovery | null,
  validation: ConnectionValidation,
  active?: PreparedPaths | null,
): Record<ConnectionRole, ConnectionPresentation> {
  const make = (role: ConnectionRole): ConnectionPresentation => {
    const selection = connectionSelection(config, role);
    const check = validation[role];
    const path = active
      ? active[role]
      : roleNeedsValidation(config, validation, role, discovery)
        ? null
        : check.path;
    const target =
      path?.target ??
      (discovery ? selectTarget(discovery, role, selection) : null);
    const observedProtocol =
      path && "fetch" in path ? path.fetch.protocol : undefined;
    const presentation =
      target && discovery
        ? describeTarget(discovery, target, observedProtocol)
        : null;
    return {
      role,
      selection,
      target,
      availability: discovery
        ? availability(discovery, role, selection)
        : "not-advertised",
      validation: active ? "verified" : check.state,
      label:
        presentation?.label ??
        (role === "throughput" ? "Throughput path" : "Latency path"),
      summary: presentation?.summary ?? "Selection unresolved",
      message: active ? undefined : check.message,
      observedProtocol,
      browserProtocol:
        path && "browserProtocol" in path ? path.browserProtocol : undefined,
      serverProtocol: path?.probe.protocolNegotiated,
      clientIp: path?.probe.clientIp,
      clientIpVersion: path?.probe.clientIpVersion,
      clientIpSource: path?.probe.clientIpSource,
      preTestPingMs: path && "rttMs" in path ? path.rttMs : undefined,
      verifiedAt: path?.verifiedAt,
    };
  };
  return { throughput: make("throughput"), latency: make("latency") };
}
export function panelReadiness(
  connections: Record<ConnectionRole, ConnectionPresentation>,
  latencyEnabled: boolean,
): ConnectionValidationState {
  const states = [
    connections.throughput.validation,
    ...(latencyEnabled ? [connections.latency.validation] : []),
  ];
  for (const state of ["failed", "checking", "stale"] as const)
    if (states.includes(state)) return state;
  return "verified";
}
