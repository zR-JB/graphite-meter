import type {
  ConnectionRole,
  InfraInfo,
  RunnerConfig,
  TransportDiscovery,
} from "./contract";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/endpoints";
import {
  selectLatencyTarget,
  selectThroughputTarget,
} from "./real/backendPure";

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
  target: FetchThroughputTarget | WebSocketLatencyTarget | null;
  availability: "advertised" | "browser-blocked" | "not-advertised";
  validation: ConnectionValidationState;
  label: string;
  summary: string;
  message?: string;
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

export function connectionSelection(
  config: RunnerConfig,
  role: ConnectionRole,
): string {
  return role === "throughput"
    ? config.transports.throughputTarget
    : config.transports.latencyTarget;
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

export function connectionKey(
  config: RunnerConfig,
  discovery?: TransportDiscovery | null,
): string {
  return JSON.stringify({
    throughput: connectionRoleKey(config, "throughput", discovery),
    latency: connectionRoleKey(config, "latency", discovery),
    needsLatency:
      config.stages.latency ||
      (!config.skipLoadedLatencyWhenStageOff &&
        (config.stages.download ||
          config.stages.upload ||
          config.stages.bidirectional)),
  });
}

export function connectionRoleKey(
  config: RunnerConfig,
  role: ConnectionRole,
  discovery?: TransportDiscovery | null,
): string {
  const selection = connectionSelection(config, role);
  const target = discovery
    ? role === "throughput"
      ? selectThroughputTarget(discovery, selection)
      : selectLatencyTarget(discovery, selection)
    : null;
  const identity = target ? JSON.stringify(target) : selection;
  return role === "throughput"
    ? identity
    : JSON.stringify({
        identity,
        needed:
          config.stages.latency ||
          (!config.skipLoadedLatencyWhenStageOff &&
            (config.stages.download ||
              config.stages.upload ||
              config.stages.bidirectional)),
      });
}

function protocolLabel(protocol: string): string {
  if (protocol === "http1") return "HTTP/1.1";
  if (protocol === "http2") return "HTTP/2";
  if (protocol === "http3") return "HTTP/3";
  return protocol;
}

function targetSummary(
  target: FetchThroughputTarget | WebSocketLatencyTarget,
): string {
  const mechanism =
    target.transport === "websocket" ? "WebSocket" : "Fetch stream";
  return `${mechanism} · ${protocolLabel(target.protocol)} · ${target.tls ? "TLS" : "clear"}`;
}

function targetLabel(
  role: ConnectionRole,
  target: FetchThroughputTarget | WebSocketLatencyTarget | null,
): string {
  if (!target)
    return role === "throughput" ? "Throughput path" : "Latency path";
  if (target.transport === "websocket")
    return target.tls ? "Secure WebSocket" : "Clear WebSocket";
  return protocolLabel(target.protocol);
}

function availability(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  selection: string,
): ConnectionPresentation["availability"] {
  if (selection === "current" || selection === "auto")
    return role === "throughput"
      ? selectThroughputTarget(discovery, selection)
        ? "advertised"
        : "not-advertised"
      : selectLatencyTarget(discovery, selection)
        ? "advertised"
        : "not-advertised";
  return discovery[role][selection]?.state ?? "not-advertised";
}

export function presentConnections(
  config: RunnerConfig,
  discovery: TransportDiscovery | null,
  validation: ConnectionValidation,
  infra: InfraInfo | null,
): Record<ConnectionRole, ConnectionPresentation> {
  const make = (role: ConnectionRole): ConnectionPresentation => {
    const selection =
      role === "throughput"
        ? config.transports.throughputTarget
        : config.transports.latencyTarget;
    const target = discovery
      ? role === "throughput"
        ? selectThroughputTarget(discovery, selection)
        : selectLatencyTarget(discovery, selection)
      : null;
    const status = validation[role];
    const evidenceMatches = status.identity
      ? status.identity === connectionRoleKey(config, role, discovery)
      : status.selection === selection;
    const currentEvidence =
      status.state === "verified" &&
      evidenceMatches &&
      infra?.discoveryGeneration === discovery?.generation;
    const evidence = currentEvidence ? infra : null;
    return {
      role,
      selection,
      target,
      availability: discovery
        ? availability(discovery, role, selection)
        : "not-advertised",
      validation: status.state,
      label: targetLabel(role, target),
      summary: target ? targetSummary(target) : "Selection unresolved",
      message: status.message,
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
