import type {
  ConnectionRole,
  DiscoveredTarget,
  InfraInfo,
  ProtocolTarget,
  RunnerConfig,
  RunnerError,
  TransportDiscovery,
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

export interface RoleValidation {
  selection: string;
  identity?: string;
  state: ConnectionValidationState;
  verifiedAt?: number;
  message?: string;
}

export interface ConnectionValidation {
  throughput: RoleValidation;
  latency: RoleValidation;
}

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

export const CONNECTION_FRESH_MS = 30_000;
export const CONNECTION_ROLES: ConnectionRole[] = ["throughput", "latency"];

/** Failures that leave the path's reachability unknown: connectivity latches
 *  offline and the cached probe is dropped so both roles are re-checked. */
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

/** Whether the run opens a latency path at all: the idle latency stage, or a
 *  transfer stage whose loaded pings are not suppressed. A latency selection
 *  that never opens must not invalidate a cached preparation. */
function latencyPathNeeded(config: RunnerConfig): boolean {
  return (
    config.stages.latency ||
    (!config.skipLoadedLatencyWhenStageOff &&
      (config.stages.download ||
        config.stages.upload ||
        config.stages.bidirectional))
  );
}

function selectTarget(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  selection: string,
): FetchThroughputTarget | WebTransportThroughputTarget | LatencyTarget | null {
  // The panel must resolve what the runner resolves, so it applies the same
  // browser-capability gate rather than the parameter's off default.
  return role === "throughput"
    ? selectThroughputTarget(discovery, selection)
    : selectLatencyTarget(
        discovery,
        selection,
        typeof WebTransport !== "undefined",
      );
}

export function validationRoles(
  config: RunnerConfig,
  validation: ConnectionValidation,
  requestedRole?: ConnectionRole,
  discovery?: TransportDiscovery | null,
): ConnectionRole[] {
  const roles = requestedRole ? [requestedRole] : [...CONNECTION_ROLES];
  for (const role of CONNECTION_ROLES) {
    const status = validation[role];
    const matches = status.identity
      ? status.identity === connectionRoleKey(config, role, discovery)
      : status.selection === connectionSelection(config, role);
    if (!roles.includes(role) && (status.state === "stale" || !matches))
      roles.push(role);
  }
  return roles;
}

export function verifiedRolesForProbe(
  requested: ConnectionRole[],
  discoveryGeneration: string | undefined,
  resultGeneration: string,
): ConnectionRole[] {
  return discoveryGeneration === resultGeneration
    ? requested
    : [...CONNECTION_ROLES];
}

export function connectionKey(
  config: RunnerConfig,
  discovery?: TransportDiscovery | null,
): string {
  return JSON.stringify({
    throughput: connectionRoleKey(config, "throughput", discovery),
    latency: connectionRoleKey(config, "latency", discovery),
    needsLatency: latencyPathNeeded(config),
  });
}

/** Inputs that can invalidate preparation without depending on discovery or
 * runtime protocol evidence produced by that same preparation. */
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
  return JSON.stringify({
    throughput: connectionDraftRoleKey(config, "throughput"),
    latency: connectionDraftRoleKey(config, "latency"),
  });
}

export function connectionRoleKey(
  config: RunnerConfig,
  role: ConnectionRole,
  discovery?: TransportDiscovery | null,
): string {
  const selection = connectionSelection(config, role);
  const target = discovery ? selectTarget(discovery, role, selection) : null;
  return target ? JSON.stringify(target) : selection;
}

/** Whether a role has to be checked now: one the run never opens does not, and
 *  one already verified against the same target does not either. This is what
 *  keeps a stage toggle from re-checking a path that has not changed. */
export function roleNeedsValidation(
  config: RunnerConfig,
  validation: ConnectionValidation,
  role: ConnectionRole,
  discovery?: TransportDiscovery | null,
): boolean {
  if (role === "latency" && !latencyPathNeeded(config)) return false;
  const status = validation[role];
  if (status.state !== "verified") return true;
  return status.identity !== connectionRoleKey(config, role, discovery);
}

function availability(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  selection: string,
): ConnectionPresentation["availability"] {
  // "current"/"auto" have no entry of their own: they resolve to whichever
  // advertised target the selector picks. Any other selection names one
  // mechanism and shares the state of the origin carrying it.
  if (selection !== "current" && selection !== "auto") {
    const byOrigin: Record<
      string,
      DiscoveredTarget<{ id: string }>
    > = discovery[role];
    return (
      locateTarget(byOrigin, selection)?.entry.state ??
      byOrigin[selection]?.state ??
      "not-advertised"
    );
  }
  return selectTarget(discovery, role, selection)
    ? "advertised"
    : "not-advertised";
}

export function presentConnections(
  config: RunnerConfig,
  discovery: TransportDiscovery | null,
  validation: ConnectionValidation,
  infra: InfraInfo | null,
): Record<ConnectionRole, ConnectionPresentation> {
  const make = (role: ConnectionRole): ConnectionPresentation => {
    const selection = connectionSelection(config, role);
    const target = discovery ? selectTarget(discovery, role, selection) : null;
    const status = validation[role];
    const evidenceMatches = status.identity
      ? status.identity === connectionRoleKey(config, role, discovery)
      : status.selection === selection;
    const currentEvidence =
      status.state === "verified" &&
      evidenceMatches &&
      infra?.discoveryGeneration === discovery?.generation;
    const evidence = currentEvidence ? infra : null;
    const observedProtocol =
      evidence && role === "throughput"
        ? evidence.selectedThroughputProtocol
        : undefined;
    const targetPresentation =
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
      validation: status.state,
      label:
        targetPresentation?.label ??
        (role === "throughput" ? "Throughput path" : "Latency path"),
      summary: targetPresentation?.summary ?? "Selection unresolved",
      message: status.message,
      observedProtocol,
      browserProtocol: evidence
        ? role === "throughput"
          ? evidence.firstHopProtocol
          : undefined
        : undefined,
      serverProtocol: evidence
        ? role === "throughput"
          ? evidence.protocolNegotiated
          : evidence.latencyProtocolNegotiated
        : undefined,
      clientIp: evidence
        ? role === "throughput"
          ? evidence.clientIp
          : evidence.latencyClientIp
        : undefined,
      clientIpVersion: evidence
        ? role === "throughput"
          ? evidence.clientIpVersion
          : evidence.latencyClientIpVersion
        : undefined,
      clientIpSource: evidence
        ? role === "throughput"
          ? evidence.clientIpSource
          : evidence.latencyClientIpSource
        : undefined,
      preTestPingMs:
        evidence && role === "latency" ? evidence.preTestPingMs : undefined,
      verifiedAt: currentEvidence ? status.verifiedAt : undefined,
    };
  };
  return { throughput: make("throughput"), latency: make("latency") };
}

/** The one state a panel covering both roles reports: the worst across the
 *  roles the run opens. A failure must not read as a check still in flight. */
export function panelReadiness(
  connections: Record<ConnectionRole, ConnectionPresentation>,
  latencyEnabled: boolean,
): ConnectionValidationState {
  const states = [
    connections.throughput.validation,
    ...(latencyEnabled ? [connections.latency.validation] : []),
  ];
  for (const state of ["checking", "failed", "stale"] as const)
    if (states.includes(state)) return state;
  return "verified";
}
