// Paths in words: how a target reads in Settings and Details, the choices each picker offers on the selected servers
// and why one cannot be chosen, and the line that states a role's check.
import type { ServerView } from "../../engine/net/connection";
import type { PageConfig } from "../../engine/net/page";
import { parseSelection, pointsAt, PROTOCOL_LABEL, resolve, type Selection } from "../../engine/net/paths";
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
import { READINESS, TRANSPORT, transportLabel, type Word } from "../../view/words";

type Browser = Pick<PageConfig, "origin" | "secure" | "webTransport">;
type Target = ThroughputTarget | LatencyTarget;
/** What a description reads of a target; a run's summary carries no more. */
export interface PathTarget {
  transport: string;
  origin: string;
  tls: boolean;
  protocol?: HttpProtocol;
}

/** The mechanism, its carrier, and both together: "Fetch streams over HTTP/1.1 (TLS)". A WebSocket names HTTP/1.1
    only when its origin's fetch target does. */
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
export function shownTarget(view: ServerView | undefined, role: Role, page: Browser): Target | null {
  if (!view) return null;
  const path = view[role].path;
  if (path) return path.target;
  return view.discovery ? resolve(view.discovery, role, parseSelection(view[role].selection), page) : null;
}

export const pathSummary = (discovery: Discovery | null, role: Role, target: PathTarget | null) =>
  target ? describePath(discovery, role, target).summary : "Selection unresolved";

const origins = (discovery: Discovery, role: Role) => discovery[role] as ReadonlyMap<string, OriginTargets<Target>>;

/** The browser's reason, when its policy excludes every origin the choice points at. */
function blockedReason(discovery: Discovery, role: Role, selection: Selection): string | null {
  let reason: string | null = null;
  for (const entry of origins(discovery, role).values()) {
    if (!entry.targets.some((target) => pointsAt(selection, target))) continue;
    if (!entry.blocked) return null;
    reason ??= entry.blocked;
  }
  return reason;
}

/** Why one server cannot run `value` for `role`; null when it can. */
function unavailable(discovery: Discovery | null, role: Role, value: string, page: Browser): string | null {
  if (!discovery) return "Checking server transports…";
  const selection = parseSelection(value);
  const blocked = blockedReason(discovery, role, selection);
  if (blocked) return blocked;
  if (resolve(discovery, role, selection, page)) return null;
  if (resolve(discovery, role, selection, { ...page, webTransport: "available" }))
    return page.webTransport === "insecure-page"
      ? "Needs a secure page: browsers offer WebTransport over HTTPS only — reopen this page on its https:// address"
      : "WebTransport is unavailable in this browser";
  if (selection.kind === "auto")
    return role === "throughput"
      ? "No offered throughput path works in this browser"
      : `No ${page.secure ? "secure" : "clear"} WebSocket offered by this server`;
  return "Not offered by this server";
}

export interface Choice {
  /** The saved selection the choice writes. */
  value: string;
  label: string;
  /** The choice group whose note the row shows. */
  group: string;
  /** Why it cannot be chosen; null when it can. */
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
  target.transport === "fetch-stream" ? `protocol:${target.protocol}` : `transport:${target.transport}`;

/** Every choice a picker offers. One server offers Automatic, the chosen group, and each mechanism it advertises,
    a negotiated one named by the protocol its verified path saw; several servers offer the groups, each available
    only when every server that answers (all of them when none does) can run it. Datagram throughput shows only
    when its switch is on or it is chosen. */
export function choices(
  role: Role,
  servers: readonly ServerIdentity[],
  views: ReadonlyMap<string, ServerView>,
  config: TestConfig,
  page: Browser,
): Choice[] {
  const selected = config.transports[TRANSPORT_KEY[role]];
  const datagrams = role === "throughput" && (config.experimentalDatagramThroughput || selected === DATAGRAMS);
  const groups = datagrams ? [...GROUPS[role], [DATAGRAMS, TRANSPORT["webtransport-datagram"]] as const] : GROUPS[role];
  if (servers.length <= 1) {
    const view = servers[0] && views.get(servers[0].id);
    const known = view?.discovery ?? null;
    const observed = view?.[role].path?.target;
    const targets = known
      ? [...origins(known, role).values()]
          .flatMap((entry) => entry.targets)
          .filter((target) => target.transport !== "webtransport-datagram" || datagrams || target.id === selected)
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
  const listed = groups.some(([value]) => value === selected)
    ? groups
    : [...groups, [selected, "Selected origin"] as const];
  // A failed server's last discovery says nothing about today.
  const reachable = servers.filter((server) => views.get(server.id)?.readiness !== "failed");
  const counted = reachable.length ? reachable : servers;
  const names = (list: readonly ServerIdentity[]) => list.map((server) => server.name).join(", ");
  return listed.map(([value, label]) => {
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

export type RoleState = ServerView["throughput"]["state"];

/** The check line under a picker: one server's own state and words, or the worst of several and how many are
    ready; a failure's message stands in for its status word. Null when no server is left to state. */
export function roleLine(
  role: Role,
  considered: readonly ServerView[],
  several: boolean,
  page: Browser,
): { state: RoleState; status: Word; summary: string | null } | null {
  const [first] = considered;
  if (!first) return null;
  if (!several) {
    const { state, message } = first[role];
    if (state === "failed" && message) return { state, status: { label: message, tone: "err" }, summary: null };
    return {
      state,
      status: READINESS[state],
      summary: message ?? pathSummary(first.discovery, role, shownTarget(first, role, page)),
    };
  }
  const states = considered.map((view) => view[role].state);
  const state = (["checking", "failed", "unchecked"] as const).find((each) => states.includes(each)) ?? "verified";
  const verified = states.filter((each) => each === "verified").length;
  return {
    state,
    status: READINESS[state],
    summary: verified === states.length ? "Resolved per server" : `${verified} of ${states.length} servers ready`,
  };
}
