import type { ServerCatalog, ServerIdentity } from "../servers/catalog";

export function serverAccent(
  server: ServerIdentity,
  catalog: readonly ServerIdentity[],
): string {
  let index = catalog.findIndex(
    (entry) => entry.id === server.id && entry.url === server.url,
  );
  if (index < 0) {
    index = 0;
    for (const char of server.id + server.url)
      index = (Math.imul(index, 31) + char.charCodeAt(0)) >>> 0;
  }
  // Muted identity tones stay separate from the measurement phase tokens.
  return `hsl(${(205 + index * 137.508) % 360} 16% 59%)`;
}

export function serverLabel(
  server: Pick<ServerIdentity, "name" | "location">,
): string {
  return server.location &&
    !server.name.toLowerCase().includes(server.location.toLowerCase())
    ? `${server.name} · ${server.location}`
    : server.name;
}

export function serverName(
  selection: readonly { id: string; name: string }[],
  id: string,
): string {
  return selection.find((server) => server.id === id)?.name ?? "Server";
}

export function catalogSelection(
  catalog: ServerCatalog | null,
  ids: readonly string[],
) {
  return catalog?.servers.filter((server) => ids.includes(server.id)) ?? [];
}
