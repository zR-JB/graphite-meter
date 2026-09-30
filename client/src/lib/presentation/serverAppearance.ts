import type { ServerCatalog, ServerIdentity } from "../servers/catalog";

export function serverLabel(
  server: Pick<ServerIdentity, "name" | "location">,
): string {
  const name = server.name.toLowerCase();
  const location = server.location?.toLowerCase() ?? "";
  // A city-named server reads once: "Nuremberg, DE", not "Nuremberg, Nuremberg, DE".
  if (!location || name.includes(location)) return server.name;
  return location.startsWith(name)
    ? server.location!
    : `${server.name}, ${server.location}`;
}

/** The all-servers choice counts who stayed to the end, or, when none did, who measured anything. */
export function allServersLabel(
  total: number,
  stayed: number,
  measured: number,
): string {
  const shown = stayed || measured;
  return shown < total
    ? `${shown} of ${total} servers`
    : `All ${total} servers`;
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
