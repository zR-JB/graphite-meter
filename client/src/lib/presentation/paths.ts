// Connection paths in words: labels, option availability and verification cards.
import type {
  ConnectionRole,
  DiscoveredTarget,
  PreparedPaths,
  ProtocolTarget,
  RunnerConfig,
  TransportDiscovery,
} from "../runner/contract";
import {
  blockedSelectionReason,
  candidates,
  connectionSelection,
  httpProtocolLabel,
  locateTarget,
  selectTarget,
  webTransportGap,
  type AnyTarget,
  type ConnectionValidation,
  type ConnectionValidationState,
} from "../runner/paths";
import { isLoopbackHostname, type ServerIdentity } from "../servers/catalog";
import { TRANSPORT } from "./vocabulary";

const NOT_ADVERTISED = "Not offered in /preflight.";
const H3 = httpProtocolLabel("http3");

/** Compact label, connection-row summary and settings detail for one target. */
export function describeTarget(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  target: AnyTarget,
  observed?: ProtocolTarget,
): { label: string; summary: string; advertisedDetail: string } {
  const security = target.tls ? "TLS" : "clear";
  const at = target.origin;
  if (target.transport === "webtransport-datagram")
    return {
      label: `${TRANSPORT["webtransport-datagram"]} · ${security}`,
      summary: `${TRANSPORT["webtransport-datagram"]} · ${H3} · ${security}`,
      advertisedDetail: `Experimental unreliable-datagram flood over ${H3} · ${at}`,
    };
  if (target.transport === "webtransport")
    return role === "throughput"
      ? {
          label: `WebTransport · ${H3} · ${security}`,
          summary: `${TRANSPORT.webtransport} · ${H3} · ${security}`,
          advertisedDetail: `QUIC stream session over ${H3} · ${at}`,
        }
      : {
          label: `WebTransport · ${H3} · ${security}`,
          summary: `WebTransport · ${H3} datagrams · ${security}`,
          advertisedDetail: `Datagram bus over ${H3} · ${at}`,
        };
  if (target.transport === "websocket") {
    // A WebSocket claims HTTP/1.1 only when its origin's fetch target names that protocol.
    const h1 = discovery.throughput[at]?.targets.some(
      (t) => t.transport === "fetch-stream" && t.protocol === "http1",
    );
    const mechanism = h1
      ? `WebSocket · ${httpProtocolLabel("http1")}`
      : "WebSocket";
    return {
      label: `${mechanism} · ${security}`,
      summary: `${mechanism} · ${security}`,
      advertisedDetail: `${h1 ? `Direct ${httpProtocolLabel("http1")} WebSocket` : "WebSocket"} endpoint · ${at}`,
    };
  }
  const protocol = httpProtocolLabel(
    target.protocol === "negotiated" && observed ? observed : target.protocol,
  );
  return {
    label: `${protocol} · ${security}`,
    summary: `${TRANSPORT["fetch-stream"]} · ${protocol} · ${security}`,
    advertisedDetail:
      target.protocol === "negotiated"
        ? `Browser negotiates the available HTTP version · ${at}`
        : `Direct ${httpProtocolLabel(target.protocol)} endpoint · ${at}`,
  };
}

export interface PathOption {
  value: string;
  label: string;
  disabled: boolean;
  detail: string;
}

const noWebTransport = () =>
  webTransportGap() === "insecure-page"
    ? "Needs a secure page: browsers offer WebTransport over HTTPS only — reopen this page on its https:// address."
    : "WebTransport is unavailable in this browser.";

/** Why one server's discovery can or cannot drive a selection, as a run would resolve it. */
function availability(
  discovery: TransportDiscovery | null,
  role: ConnectionRole,
  value: string,
) {
  if (!discovery)
    return { disabled: true, detail: "Checking server transports…" };
  const restriction = blockedSelectionReason(discovery, role, value);
  if (restriction) return { disabled: true, detail: restriction };
  const target = selectTarget(discovery, role, value);
  const byOrigin: Record<string, DiscoveredTarget<AnyTarget>> = discovery[role];
  const found = locateTarget(byOrigin, value);
  const entry = found?.entry ?? byOrigin[value];
  if (target && value === "auto") {
    const first = `${describeTarget(discovery, role, target).summary} · ${target.origin}`;
    return {
      disabled: false,
      detail:
        candidates(discovery, role).length > 1
          ? `Tries ${first} first, then verifies advertised alternatives.`
          : `Checks ${first}.`,
    };
  }
  if (target) {
    const loopback =
      discovery.pageSecure &&
      !target.tls &&
      isLoopbackHostname(new URL(target.origin).hostname);
    const detail = describeTarget(discovery, role, target).advertisedDetail;
    return {
      disabled: false,
      detail: loopback ? `Clear loopback endpoint · ${target.origin}` : detail,
    };
  }
  if (selectTarget(discovery, role, value, true))
    return { disabled: true, detail: noWebTransport() };
  if (value === "auto")
    return {
      disabled: true,
      detail:
        role === "throughput"
          ? "No advertised throughput path is usable in this browser."
          : `${discovery.pageSecure ? "Secure" : "Clear"} WebSocket target is not offered in /preflight.`,
    };
  const blocked = entry?.state === "browser-blocked";
  return {
    disabled: true,
    detail: blocked
      ? (entry.blockedReason ??
        `Blocked by the browser: a secure page cannot open this clear endpoint · ${entry.targets[0]?.origin}`)
      : NOT_ADVERTISED,
  };
}

/** Every choice a picker offers, with its availability on each selected server. */
export function pathOptions(
  role: ConnectionRole,
  servers: readonly ServerIdentity[],
  views: ReadonlyMap<string, { discovery: TransportDiscovery | null }>,
  config: RunnerConfig,
  observed?: { id?: string; protocol?: ProtocolTarget },
  simultaneous = servers.length > 1,
): PathOption[] {
  const selected = connectionSelection(config, role);
  const datagrams =
    role === "throughput" &&
    (config.experimentalDatagramThroughput ||
      selected === "transport:webtransport-datagram");
  const groups: [string, string][] =
    role === "throughput"
      ? [
          ["auto", "Automatic"],
          ["protocol:http1", "HTTP/1.1"],
          ["protocol:http2", "HTTP/2"],
          ["protocol:http3", "HTTP/3"],
          ["transport:webtransport", TRANSPORT.webtransport],
          ...(datagrams
            ? [
                [
                  "transport:webtransport-datagram",
                  TRANSPORT["webtransport-datagram"],
                ] as [string, string],
              ]
            : []),
        ]
      : [
          ["auto", "Automatic"],
          ["transport:websocket", "WebSocket"],
          ["transport:webtransport", "WebTransport"],
        ];
  const discovery = (id: string) => views.get(id)?.discovery ?? null;
  if (!simultaneous) {
    // One server offers one card per advertised mechanism, and keeps a selected group visible.
    const known = servers[0] ? discovery(servers[0].id) : null;
    const targets = Object.values<DiscoveredTarget<AnyTarget>>(
      known?.[role] ?? {},
    )
      .flatMap((entry) => entry.targets)
      .filter(
        (target) =>
          target.transport !== "webtransport-datagram" ||
          datagrams ||
          selected === target.id,
      );
    const single = [
      groups[0],
      ...groups.filter(([value]) => value !== "auto" && value === selected),
      ...targets.map((target): [string, string] => [
        target.id,
        describeTarget(
          known!,
          role,
          target,
          observed?.id === target.id ? observed.protocol : undefined,
        ).label,
      ]),
    ];
    return single.map(([value, label]) => ({
      value,
      label,
      ...availability(known, role, value),
    }));
  }
  if (!groups.some(([value]) => value === selected))
    groups.push([selected, "Selected origin"]);
  return groups.map(([value, label]) => {
    const missing = servers.filter((server) => !discovery(server.id));
    const incompatible = servers.filter(
      (server) =>
        discovery(server.id) &&
        availability(discovery(server.id), role, value).disabled,
    );
    const restrictions = incompatible.flatMap((server) => {
      const reason = blockedSelectionReason(discovery(server.id)!, role, value);
      return reason ? [`${server.name}: ${reason}`] : [];
    });
    const names = (list: readonly ServerIdentity[]) =>
      list.map((server) => server.name).join(", ");
    const detail = restrictions.length
      ? restrictions.join(" ")
      : value === "auto"
        ? role === "throughput"
          ? "Prefers HTTP/1.1 streams, then HTTP/2 and HTTP/3; verifies fallbacks per server"
          : "Prefers WebTransport datagrams; verifies WebSocket fallbacks per server"
        : incompatible.length
          ? `Unavailable on ${names(incompatible)}`
          : missing.length
            ? `Checking ${names(missing)}`
            : `Available on all ${servers.length} selected servers`;
    return {
      value,
      label,
      detail,
      disabled:
        value !== "auto" && (missing.length > 0 || incompatible.length > 0),
    };
  });
}

export interface ConnectionPresentation {
  role: ConnectionRole;
  selection: string;
  target: AnyTarget | null;
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

/** A server view's path per role, or the run's own paths, as the connection rows and details show it. */
export function presentConnections(
  config: RunnerConfig,
  discovery: TransportDiscovery | null,
  validation: ConnectionValidation,
  active?: PreparedPaths | null,
): Record<ConnectionRole, ConnectionPresentation> {
  const make = (role: ConnectionRole): ConnectionPresentation => {
    const selection = connectionSelection(config, role);
    const check = validation[role];
    const path = active ? active[role] : check.path;
    const target =
      path?.target ??
      (discovery ? selectTarget(discovery, role, selection) : null);
    const observedProtocol =
      path && "fetch" in path ? path.fetch.protocol : undefined;
    const described =
      target && discovery
        ? describeTarget(discovery, role, target, observedProtocol)
        : null;
    const byOrigin: Record<string, DiscoveredTarget<AnyTarget>> = discovery?.[
      role
    ] ?? {};
    const state =
      discovery &&
      (selection === "auto" ||
        (selection.includes(":") && !selection.includes("://")))
        ? target
          ? "advertised"
          : blockedSelectionReason(discovery, role, selection)
            ? "browser-blocked"
            : "not-advertised"
        : (locateTarget(byOrigin, selection)?.entry.state ??
          byOrigin[selection]?.state ??
          "not-advertised");
    return {
      role,
      selection,
      target,
      availability: state,
      validation: active ? "verified" : check.state,
      label:
        described?.label ??
        (role === "throughput" ? "Throughput path" : "Latency path"),
      summary: described?.summary ?? "Selection unresolved",
      message: active ? undefined : check.message,
      observedProtocol,
      browserProtocol:
        path && "browserProtocol" in path ? path.browserProtocol : undefined,
      serverProtocol: path?.probe.protocolNegotiated,
      clientIp: path?.probe.clientIp,
      clientIpVersion: path?.probe.clientIpVersion,
      clientIpSource: path?.probe.clientIpSource,
      preTestPingMs:
        path && "rttMs" in path ? (path.rttMs ?? undefined) : undefined,
      verifiedAt: path?.verifiedAt,
    };
  };
  return { throughput: make("throughput"), latency: make("latency") };
}
