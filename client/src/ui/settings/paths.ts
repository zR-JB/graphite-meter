// Paths in words for Settings and Details, the choices each picker offers and why one cannot be chosen.
import type { ServerView } from "../../engine/net/connection";
import type { PageConfig } from "../../engine/net/page";
import {
  blockedReason,
  parseSelection,
  PROTOCOL_LABEL,
  resolve,
  unresolvedReason,
} from "../../engine/net/paths";
import { TRANSPORT_KEY } from "../../engine/net/servers";
import type {
  Discovery,
  HttpProtocol,
  LatencyTarget,
  OriginTargets,
  Role,
  ServerIdentity,
  TestConfig,
  ThroughputTarget,
} from "../../engine/types";
import { listed, READINESS, TRANSPORT, transportLabel, type Word } from "../../view/words";

type Browser = Pick<PageConfig, "origin" | "secure" | "webTransport">;
type Target = ThroughputTarget | LatencyTarget;
/** What a description reads of a target; a run's summary carries no more. */
interface PathTarget {
  transport: string;
  origin: string;
  tls: boolean;
  protocol?: HttpProtocol;
}

/** The mechanism and its carrier together: "Fetch streams over HTTP/1.1 (TLS)"; a WebSocket names HTTP/1.1 only when its origin's fetch target does. */
export function describePath(
  discovery: Discovery | null,
  role: Role,
  target: PathTarget,
): { label: string; summary: string; carrier: string } {
  const security = target.tls ? "TLS" : "clear";
  const secured = (carrier: string) => `${carrier} (${security})`;
  const mechanism = transportLabel(target.transport, role) ?? target.transport;
  if (target.transport === "websocket") {
    const h1 = discovery?.throughput
      .get(target.origin)
      ?.targets.some((each) => each.transport === "fetch-stream" && each.protocol === "http1");
    const carrier = h1 ? secured(PROTOCOL_LABEL.http1) : security;
    const label = h1 ? `${mechanism} over ${carrier}` : secured(mechanism);
    return { label, summary: label, carrier };
  }
  if (target.transport !== "fetch-stream") {
    const carrier = secured(PROTOCOL_LABEL.http3);
    return { label: secured(mechanism), summary: `${mechanism} over ${carrier}`, carrier };
  }
  const carrier = secured(PROTOCOL_LABEL[target.protocol ?? "negotiated"]);
  return { label: carrier, summary: `${mechanism} over ${carrier}`, carrier };
}

/** The target a role uses on one server now: its verified path's, else what the saved choice resolves to. */
export function shownTarget(
  view: ServerView | undefined,
  role: Role,
  page: Browser,
): Target | null {
  if (!view) return null;
  const path = view[role].path;
  if (path) return path.target;
  return view.discovery
    ? resolve(view.discovery, role, parseSelection(view[role].selection), page)
    : null;
}

export const pathSummary = (discovery: Discovery | null, role: Role, target: PathTarget | null) =>
  target ? describePath(discovery, role, target).summary : "Selection unresolved";

const origins = (discovery: Discovery, role: Role) =>
  discovery[role] as ReadonlyMap<string, OriginTargets<Target>>;

function unavailable(
  discovery: Discovery | null,
  role: Role,
  value: string,
  page: Browser,
): string | null {
  if (!discovery) return "Checking server transports…";
  const selection = parseSelection(value);
  return resolve(discovery, role, selection, page)
    ? null
    : unresolvedReason(discovery, role, selection, page).message;
}

export interface Choice {
  value: string;
  label: string;
  /** The choice group whose note the row shows. */
  group: string;
  unavailable: string | null;
}

const DATAGRAMS = "transport:webtransport-datagram";
const GROUPS: Record<Role, readonly (readonly [string, string])[]> = {
  throughput: [
    ["auto", "Automatic"],
    ["protocol:http1", PROTOCOL_LABEL.http1],
    ["protocol:http2", PROTOCOL_LABEL.http2],
    ["protocol:http3", PROTOCOL_LABEL.http3],
    ["transport:webtransport", TRANSPORT.webtransport],
  ],
  latency: [
    ["auto", "Automatic"],
    ["transport:websocket", TRANSPORT.websocket],
    ["transport:webtransport", TRANSPORT["webtransport-datagram"]],
  ],
};

const groupOf = (target: Target) =>
  target.transport === "fetch-stream"
    ? `protocol:${target.protocol}`
    : `transport:${target.transport}`;

/** Every choice a picker offers: one server's advertised mechanisms, or for several the groups every answering server can run. */
export function choices(
  role: Role,
  servers: readonly ServerIdentity[],
  views: ReadonlyMap<string, ServerView>,
  config: TestConfig,
  page: Browser,
): Choice[] {
  const selected = config.transports[TRANSPORT_KEY[role]];
  const datagrams =
    role === "throughput" && (config.experimentalDatagramThroughput || selected === DATAGRAMS);
  const groups = datagrams
    ? [...GROUPS[role], [DATAGRAMS, TRANSPORT["webtransport-datagram"]] as const]
    : GROUPS[role];
  if (servers.length <= 1) {
    const view = servers[0] && views.get(servers[0].id);
    const known = view?.discovery ?? null;
    const observed = view?.[role].path?.target;
    const targets = known
      ? [...origins(known, role).values()]
          .flatMap((entry) => entry.targets)
          .filter(
            (target) =>
              target.transport !== "webtransport-datagram" || datagrams || target.id === selected,
          )
      : [];
    const rows = [
      ...groups
        .filter(([value]) => value === "auto" || value === selected)
        .map(([value, label]) => ({ value, label, group: value })),
      ...targets.map((target) => ({
        value: target.id,
        label: describePath(known, role, observed?.id === target.id ? observed : target).label,
        group: groupOf(target),
      })),
    ];
    return rows.map((row) => ({ ...row, unavailable: unavailable(known, role, row.value, page) }));
  }
  const offered = groups.some(([value]) => value === selected)
    ? groups
    : [...groups, [selected, "Selected origin"] as const];
  // A failed server's last discovery says nothing about today.
  const reachable = servers.filter((server) => views.get(server.id)?.readiness !== "failed");
  const counted = reachable.length ? reachable : servers;
  const names = (list: readonly ServerIdentity[]) => listed(list.map((server) => server.name));
  return offered.map(([value, label]) => {
    const missing = counted.filter((server) => !views.get(server.id)?.discovery);
    const lacking = counted.filter((server) => {
      const discovery = views.get(server.id)?.discovery;
      return discovery && unavailable(discovery, role, value, page) !== null;
    });
    const restrictions = lacking.flatMap((server) => {
      const reason = blockedReason(views.get(server.id)!.discovery!, role, parseSelection(value));
      return reason ? [`${server.name}: ${reason}`] : [];
    });
    const why =
      value === "auto" || (!missing.length && !lacking.length)
        ? null
        : restrictions.length
          ? restrictions.join(" ")
          : lacking.length
            ? `Unavailable on ${names(lacking)}`
            : `Checking ${names(missing)}`;
    return { value, label, group: value, unavailable: why };
  });
}

type RoleState = ServerView["throughput"]["state"];

/** The check line under a picker: one server's state, or the worst of several and how many are ready; null with no server left. */
export function roleLine(
  role: Role,
  considered: readonly ServerView[],
  several: boolean,
  page: Browser,
): { state: RoleState; status: Word; summary: string | null } | null {
  const [first] = considered;
  if (!first) return null;
  if (!several) {
    const { state, message, path } = first[role];
    if (state === "failed" && message)
      return { state, status: { label: message, tone: "err" }, summary: null };
    const summary = pathSummary(first.discovery, role, shownTarget(first, role, page));
    const fallback = path?.verifiedFor.fallback;
    return {
      state,
      status: READINESS[state],
      summary:
        message ?? (fallback ? `Automatic used ${summary} for ${role}: ${fallback}` : summary),
    };
  }
  const states = considered.map((view) => view[role].state);
  const state =
    (["checking", "failed", "unchecked"] as const).find((each) => states.includes(each)) ??
    "verified";
  const verified = states.filter((each) => each === "verified").length;
  return {
    state,
    status: READINESS[state],
    summary:
      verified === states.length
        ? "Resolved per server"
        : `${verified} of ${states.length} servers ready`,
  };
}
