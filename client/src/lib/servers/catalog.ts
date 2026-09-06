import type { Preflight } from "../api/preflight";
import type { ServerEntry } from "../api/servers";
export type { ServerEntry } from "../api/servers";

export const MAX_SERVERS = 32;
export const MAX_SELECTED_SERVERS = 4;
export type ServerIdentity = Omit<ServerEntry, "additionalOrigins">;
export interface ServerCatalog {
  defaultSelection: string[];
  servers: ServerEntry[];
}
export interface SavedSelection {
  id: string;
  url: string;
}

export function canonicalOrigin(value: unknown): string {
  if (
    typeof value !== "string" ||
    value.length > 2048 ||
    !/^https?:\/\/[^@/?#\\\s*;]+\/?$/.test(value)
  )
    throw new Error("Expected an HTTP(S) origin");
  const url = new URL(value);
  if (!url.hostname || url.port === "0") throw new Error("Invalid origin");
  return url.origin;
}

/** Literal IPv6 hosts cannot be named in CSP; the page's own origin is covered by 'self'. */
export function browserOriginRestriction(
  origin: string,
  pageOrigin: string,
): string | undefined {
  const url = new URL(origin);
  if (url.hostname.startsWith("[") && url.origin !== new URL(pageOrigin).origin)
    return "Use a DNS hostname for browser connections to this IPv6 server.";
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("Invalid server catalogue");
  return value as Record<string, unknown>;
}
function text(value: unknown, maximum: number, empty = false): string {
  if (
    typeof value !== "string" ||
    (!empty && !value.length) ||
    /[\u0000-\u001f\u007f]/.test(value) ||
    new TextEncoder().encode(value).length > maximum
  )
    throw new Error("Invalid server identity");
  return value;
}
export function singletonCatalog(origin: string): ServerCatalog {
  return {
    defaultSelection: ["self"],
    servers: [
      { id: "self", url: canonicalOrigin(origin), name: "This server" },
    ],
  };
}
export function parseCatalog(value: unknown, origin: string): ServerCatalog {
  const input = object(value);
  if (
    !Array.isArray(input.servers) ||
    input.servers.length < 1 ||
    input.servers.length > MAX_SERVERS
  )
    throw new Error("Invalid server catalogue size");
  const seenIds = new Set<string>(),
    seenOrigins = new Set<string>();
  const servers = input.servers.map((raw, index): ServerEntry => {
    const row = object(raw);
    const id = text(row.id, 64);
    if (
      !/^[a-zA-Z0-9._-]+$/.test(id) ||
      (index === 0 && id !== "self") ||
      seenIds.has(id)
    )
      throw new Error("Invalid or repeated server ID");
    const url =
      row.url === "." && id === "self"
        ? canonicalOrigin(origin)
        : canonicalOrigin(row.url);
    if (seenOrigins.has(url)) throw new Error("Repeated server origin");
    const additional = row.additionalOrigins ?? [];
    if (!Array.isArray(additional) || additional.length > 32)
      throw new Error("Invalid additional origins");
    seenIds.add(id);
    seenOrigins.add(url);
    return {
      id,
      url,
      name: text(row.name, 256, true) || id,
      ...(row.location === undefined
        ? {}
        : { location: text(row.location, 256, true) }),
      additionalOrigins: additional.map(canonicalOrigin),
    };
  });
  if (!Array.isArray(input.defaultSelection))
    throw new Error("Missing default selection");
  const defaultSelection = input.defaultSelection.map((id) => text(id, 64));
  const catalog = { servers, defaultSelection };
  validateSelection(catalog, defaultSelection);
  return catalog;
}
export function validateSelection(
  catalog: ServerCatalog,
  ids: readonly string[],
): void {
  if (
    ids.length < 1 ||
    ids.length > MAX_SELECTED_SERVERS ||
    new Set(ids).size !== ids.length ||
    ids.some((id) => !catalog.servers.some((server) => server.id === id))
  )
    throw new Error("Select one to four catalogue servers");
}
export function selectedInCatalogOrder(
  catalog: ServerCatalog,
  ids: readonly string[],
): ServerEntry[] {
  validateSelection(catalog, ids);
  return catalog.servers.filter((server) => ids.includes(server.id));
}
export function reconcileSelection(
  catalog: ServerCatalog,
  saved: unknown,
): { ids: string[]; unresolved: SavedSelection[] } {
  if (
    !Array.isArray(saved) ||
    saved.length < 1 ||
    saved.length > MAX_SELECTED_SERVERS
  )
    return { ids: catalog.defaultSelection, unresolved: [] };
  const ids: string[] = [],
    unresolved: SavedSelection[] = [];
  for (const value of saved) {
    try {
      const row = object(value),
        id = text(row.id, 64),
        url = canonicalOrigin(row.url);
      if (ids.includes(id) || unresolved.some((row) => row.id === id))
        throw new Error("Repeated saved server");
      if (
        catalog.servers.some((server) => server.id === id && server.url === url)
      )
        ids.push(id);
      else unresolved.push({ id, url });
    } catch {
      return { ids: catalog.defaultSelection, unresolved: [] };
    }
  }
  return { ids, unresolved };
}
export function allowsServerOrigin(server: ServerEntry, raw: string): boolean {
  if (raw === ".") return true;
  try {
    const target = new URL(canonicalOrigin(raw)),
      base = new URL(server.url);
    return (
      target.hostname === base.hostname ||
      (server.additionalOrigins ?? []).includes(target.origin)
    );
  } catch {
    return false;
  }
}
export function validateServerDiscovery(
  server: ServerEntry,
  discovery: Preflight,
): void {
  if (
    [
      ...discovery.capabilities.throughput,
      ...discovery.capabilities.latency,
    ].some((target) => !allowsServerOrigin(server, target.baseUrl))
  )
    throw new Error(
      `${server.name} advertised an origin outside its catalogue entry`,
    );
}
export function identity(server: ServerEntry): ServerIdentity {
  return {
    id: server.id,
    url: server.url,
    name: server.name,
    ...(server.location ? { location: server.location } : {}),
  };
}
