import type { ServerCatalog, ServerIdentity } from "../servers/catalog";

export function serverLabel(
  server: Pick<ServerIdentity, "name" | "location">,
): string {
  return server.location &&
    !server.name.toLowerCase().includes(server.location.toLowerCase())
    ? `${server.name}, ${server.location}`
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
