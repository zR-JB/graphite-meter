// Which advertised target a saved path choice names on one server, and Automatic's ranking.
import type { CatalogEntry, Preflight } from "../../protocol/documents";
import type {
  Discovery,
  FetchTarget,
  HttpProtocol,
  LatencyTarget,
  OriginTargets,
  Role,
  ThroughputTarget,
  VerifiedFor,
} from "../types";
import type { PageConfig } from "./page";

type Target = ThroughputTarget | LatencyTarget;
type TargetOf<R extends Role> = R extends "throughput" ? ThroughputTarget : LatencyTarget;
type Transport = Target["transport"];
type Browser = Pick<PageConfig, "origin" | "secure" | "webTransport">;

/** A saved path choice, parsed once: Automatic, an HTTP version, a mechanism, or one origin's target. Anything else
    stays an origin that names nothing, so it round-trips and resolves to "not offered". */
export type Selection =
  | { kind: "auto" }
  | { kind: "protocol"; protocol: HttpProtocol }
  | { kind: "transport"; transport: Transport }
  | { kind: "origin"; origin: string; mechanism: "" | "::wt" | "::wtdg" };

export const PROTOCOL_LABEL: Record<HttpProtocol, string> = {
  http1: "HTTP/1.1",
  http2: "HTTP/2",
  http3: "HTTP/3",
  negotiated: "Negotiated HTTP",
};
/** In the order pickers list them. */
const TRANSPORT_LABEL: Record<Transport, string> = {
  "fetch-stream": "HTTP",
  websocket: "WebSocket",
  webtransport: "WebTransport",
  "webtransport-datagram": "WebTransport datagrams",
};
const PROTOCOLS: readonly string[] = Object.keys(PROTOCOL_LABEL);
const TRANSPORTS = Object.keys(TRANSPORT_LABEL) as Transport[];

export function parseSelection(text: string): Selection {
  if (text === "auto") return { kind: "auto" };
  const protocol = text.startsWith("protocol:") ? text.slice("protocol:".length) : "";
  if (PROTOCOLS.includes(protocol)) return { kind: "protocol", protocol: protocol as HttpProtocol };
  const transport = text.startsWith("transport:") ? text.slice("transport:".length) : "";
  if ((TRANSPORTS as string[]).includes(transport))
    return { kind: "transport", transport: transport as Transport };
  const mechanism = text.endsWith("::wtdg") ? "::wtdg" : text.endsWith("::wt") ? "::wt" : "";
  return { kind: "origin", origin: text.slice(0, text.length - mechanism.length), mechanism };
}

export function selectionText(selection: Selection): string {
  if (selection.kind === "auto") return "auto";
  if (selection.kind === "protocol") return `protocol:${selection.protocol}`;
  if (selection.kind === "transport") return `transport:${selection.transport}`;
  return selection.origin + selection.mechanism;
}

/** An explicit choice in words, for a server that does not offer it. */
function selectionName(selection: Exclude<Selection, { kind: "auto" }>): string {
  if (selection.kind === "protocol") return PROTOCOL_LABEL[selection.protocol];
  if (selection.kind === "transport") return TRANSPORT_LABEL[selection.transport];
  return selectionText(selection);
}

/** The HTTP version a Resource Timing `nextHopProtocol` names; unknown or redacted values are never guessed. */
export function protocolFromNextHop(
  nextHop: string | null,
): Exclude<HttpProtocol, "negotiated"> | null {
  return nextHop === "http/1.1"
    ? "http1"
    : nextHop === "h2"
      ? "http2"
      : nextHop === "h3"
        ? "http3"
        : null;
}

/** Whether `url` lies where `entry` may be reached: its own hostname on any scheme and port, or an exact additional
    origin. Discovery can never widen this, and it grants no credential on its own. */
export function allowsOrigin(
  entry: Pick<CatalogEntry, "url" | "additionalOrigins">,
  url: string,
): boolean {
  let target: URL;
  try {
    target = new URL(url);
  } catch {
    return false;
  }
  return (
    (target.protocol === "http:" || target.protocol === "https:") &&
    (target.hostname === new URL(entry.url).hostname ||
      entry.additionalOrigins.includes(target.origin))
  );
}

export function isLoopback(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  if (host === "localhost" || host.endsWith(".localhost") || host === "::1") return true;
  const octets = host.split(".");
  return (
    octets.length === 4 &&
    octets[0] === "127" &&
    octets.every((part) => /^\d{1,3}$/.test(part) && +part <= 255)
  );
}

/** Why this page cannot reach `origin` at all, or null. Literal IPv6 hosts cannot be named in the page's connection
    policy (its own origin is covered by 'self'); an HTTPS page reaches clear HTTP only on loopback. */
export function originPolicy(
  origin: string,
  page: Pick<PageConfig, "origin" | "secure">,
): string | null {
  const url = new URL(origin);
  if (url.hostname.startsWith("[") && url.origin !== page.origin)
    return "Use a DNS hostname for browser connections to this IPv6 server.";
  if (page.secure && url.protocol === "http:" && !isLoopback(url.hostname))
    return "Use an HTTPS origin for this server when the interface is HTTPS.";
  return null;
}

function byOrigin<T extends Target>(
  advertised: readonly { baseUrl: string; transport: string; protocol?: string }[],
  discoveryOrigin: string,
  page: Browser,
  target: (
    origin: string,
    tls: boolean,
    advertised: { transport: string; protocol?: string },
  ) => T | null,
): Map<string, OriginTargets<T>> {
  const origins = new Map<string, OriginTargets<T>>();
  for (const each of advertised) {
    const origin = each.baseUrl === "." ? discoveryOrigin : each.baseUrl;
    const next = target(origin, origin.startsWith("https:"), each);
    if (!next) continue;
    let entry = origins.get(origin);
    if (!entry) origins.set(origin, (entry = { blocked: originPolicy(origin, page), targets: [] }));
    const at = entry.targets.findIndex((known) => known.transport === next.transport);
    // One target per origin and mechanism: the first, unless a later one names the protocol a negotiated one left open.
    if (at < 0) entry.targets.push(next);
    else if (entry.targets[at]!.protocol === "negotiated") entry.targets[at] = next;
  }
  for (const entry of origins.values())
    entry.targets.sort((a, b) => TRANSPORTS.indexOf(a.transport) - TRANSPORTS.indexOf(b.transport));
  return origins;
}

/** The advertised targets grouped by origin, each origin judged once against this page. WebTransport dials only
    https: URLs, so a session target on a clear origin is not offered. */
export function classify(
  capabilities: Preflight["capabilities"],
  discoveryOrigin: string,
  page: Browser,
): Pick<Discovery, "throughput" | "latency"> {
  return {
    throughput: byOrigin<ThroughputTarget>(
      capabilities.throughput,
      discoveryOrigin,
      page,
      (origin, tls, each) => {
        if (each.transport === "fetch-stream")
          return {
            id: origin,
            origin,
            transport: "fetch-stream",
            protocol: each.protocol as HttpProtocol,
            tls,
          };
        const transport = each.transport as "webtransport" | "webtransport-datagram";
        const id = `${origin}${transport === "webtransport" ? "::wt" : "::wtdg"}`;
        return tls ? { id, origin, transport, protocol: "http3", tls } : null;
      },
    ),
    latency: byOrigin<LatencyTarget>(
      capabilities.latency,
      discoveryOrigin,
      page,
      (origin, tls, each) => {
        if (each.transport === "websocket")
          return { id: origin, origin, transport: "websocket", protocol: "http1", tls };
        return tls
          ? { id: `${origin}::wt`, origin, transport: "webtransport", protocol: "http3", tls }
          : null;
      },
    ),
  };
}

const origins = <R extends Role>(discovery: Discovery, role: R) =>
  discovery[role] as Map<string, OriginTargets<TargetOf<R>>>;

const advertised = <R extends Role>(discovery: Discovery, role: R): TargetOf<R>[] =>
  [...origins(discovery, role).values()].flatMap((entry) => (entry.blocked ? [] : entry.targets));

const runnable = (target: Target, page: Browser) =>
  page.webTransport === "available" || !target.transport.startsWith("webtransport");

const inGroup = (target: Target, selection: Selection) =>
  selection.kind === "protocol"
    ? target.transport === "fetch-stream" && target.protocol === selection.protocol
    : selection.kind === "transport" && target.transport === selection.transport;

/** Whether `selection` points at `target` at all, usable or not. */
const pointsAt = (selection: Selection, target: Target) =>
  selection.kind === "auto" ||
  inGroup(target, selection) ||
  (selection.kind === "origin" &&
    (target.id === selectionText(selection) ||
      (!selection.mechanism && target.origin === selection.origin)));

const RANK: Record<HttpProtocol, number> = { http1: 0, http2: 1, http3: 2, negotiated: 3 };

/** Automatic's candidates, best first; ties go to the discovery origin, then TLS, then the id, so the order never
    depends on how the server listed them. */
export function candidates<R extends Role>(
  discovery: Discovery,
  role: R,
  page: Browser,
): TargetOf<R>[] {
  const rank = (target: Target) => {
    if (role === "latency") return target.transport === "webtransport" ? 0 : 1;
    if (target.transport !== "fetch-stream") return 4;
    // A negotiated target on the discovery origin ranks by the protocol its preflight was seen to use.
    const proven =
      target.origin === discovery.origin ? protocolFromNextHop(discovery.protocol) : null;
    return RANK[target.protocol === "negotiated" ? (proven ?? "negotiated") : target.protocol];
  };
  const preference = (a: Target, b: Target) =>
    Number(b.origin === discovery.origin) - Number(a.origin === discovery.origin) ||
    Number(b.tls) - Number(a.tls) ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  // Datagram throughput stays an explicit choice.
  return advertised(discovery, role)
    .filter((target) => target.transport !== "webtransport-datagram" && runnable(target, page))
    .sort((a, b) => rank(a) - rank(b) || preference(a, b));
}

function locate<R extends Role>(discovery: Discovery, role: R, id: string) {
  for (const entry of origins(discovery, role).values())
    for (const target of entry.targets) if (target.id === id) return { entry, target };
  return null;
}

/** The target a run would use for `selection`, or null when nothing usable matches. An explicit choice never falls
    back to another mechanism. */
export function resolve<R extends Role>(
  discovery: Discovery,
  role: R,
  selection: Selection,
  page: Browser,
): TargetOf<R> | null {
  if (selection.kind === "auto") return candidates(discovery, role, page)[0] ?? null;
  if (selection.kind !== "origin") {
    const group = advertised(discovery, role).filter((target) => inGroup(target, selection));
    const secure = page.secure ? group.filter((target) => target.tls) : [];
    const eligible = secure.length ? secure : group;
    const target = eligible.find((each) => each.origin === discovery.origin) ?? eligible[0];
    return target && runnable(target, page) ? target : null;
  }
  const found = locate(discovery, role, selectionText(selection));
  if (found) return !found.entry.blocked && runnable(found.target, page) ? found.target : null;
  if (selection.mechanism) return null;
  // A plain origin also names an origin with exactly one usable mechanism.
  const entry = origins(discovery, role).get(selection.origin);
  const usable =
    entry && !entry.blocked ? entry.targets.filter((target) => runnable(target, page)) : [];
  return usable.length === 1 ? usable[0]! : null;
}

/** The browser's policy, when it excludes every origin `selection` points at. */
export function blockedReason(
  discovery: Discovery,
  role: Role,
  selection: Selection,
): string | null {
  let reason: string | null = null;
  for (const entry of origins(discovery, role).values()) {
    if (!entry.targets.some((target) => pointsAt(selection, target))) continue;
    if (!entry.blocked) return null;
    reason ??= entry.blocked;
  }
  return reason;
}

/** Why `selection` resolves to nothing on `discovery`, in the words the server's row and the pickers show. */
export function unresolvedReason(
  discovery: Discovery,
  role: Role,
  selection: Selection,
  page: Browser,
): { message: string; retry: "needs-user" | "impossible" } {
  const blocked = blockedReason(discovery, role, selection);
  if (blocked) return { message: blocked, retry: "impossible" };
  if (
    page.webTransport !== "available" &&
    resolve(discovery, role, selection, { ...page, webTransport: "available" })
  )
    return {
      message:
        page.webTransport === "insecure-page"
          ? "WebTransport needs a secure page; open this interface over HTTPS."
          : "This browser does not support WebTransport.",
      retry: "impossible",
    };
  if (selection.kind === "auto")
    return {
      message:
        role === "throughput"
          ? "No offered throughput path works in this browser"
          : `No ${page.secure ? "secure" : "clear"} WebSocket offered by this server`,
      retry: "impossible",
    };
  return { message: `${selectionName(selection)} not offered`, retry: "needs-user" };
}

/** Probe evidence and the upload id are HTTP whichever mechanism moves the bytes: the origin's advertised fetch
    target, else a negotiated view of it. */
export function fetchView(discovery: Discovery, target: ThroughputTarget): FetchTarget {
  const view = discovery.throughput
    .get(target.origin)
    ?.targets.find((each): each is FetchTarget => each.transport === "fetch-stream");
  return {
    ...(view ?? {
      id: target.origin,
      origin: target.origin,
      transport: "fetch-stream",
      protocol: "negotiated",
      tls: target.tls,
    }),
  };
}

/** Whether a path verified for `verifiedFor` still answers `selection`: Automatic while its first candidate is
    unchanged (a verified fallback stays good), an explicit choice while it resolves to the verified target. */
export function stillValid(
  path: { generation: string; verifiedFor: VerifiedFor },
  discovery: Discovery,
  role: Role,
  selection: Selection,
  page: Browser,
): boolean {
  return (
    path.generation === discovery.generation &&
    path.verifiedFor.selection === selectionText(selection) &&
    resolve(discovery, role, selection, page)?.id === path.verifiedFor.head
  );
}

/** A choice that applies to every server: carries a transport preference between servers without carrying another
    server's origin. It resolves as if WebTransport existed, as it converts a preference, not this browser. */
export function portable(
  role: Role,
  selection: Selection,
  discovery: Discovery | null,
  page: Browser,
): Selection {
  if (selection.kind !== "origin") return selection;
  const target =
    discovery &&
    (locate(discovery, role, selectionText(selection))?.target ??
      resolve(discovery, role, selection, { ...page, webTransport: "available" }));
  if (!target) {
    if (selection.mechanism === "::wt") return { kind: "transport", transport: "webtransport" };
    if (role === "throughput" && selection.mechanism === "::wtdg")
      return { kind: "transport", transport: "webtransport-datagram" };
    return { kind: "auto" };
  }
  if (target.transport !== "fetch-stream")
    return { kind: "transport", transport: target.transport };
  return target.protocol === "negotiated"
    ? { kind: "auto" }
    : { kind: "protocol", protocol: target.protocol };
}
