import { serverLabel } from "../presentation/serverAppearance";
import type { HistoryRecord } from "./types";

/** Saved selection includes earlier participants even if they left a partial run. */
export function historyServers(
  record: Pick<HistoryRecord, "server" | "multiServer">,
) {
  const details = record.multiServer;
  if (!details)
    return [
      {
        id: "single",
        label: serverLabel({
          name: record.server.name,
          location: record.server.location ?? undefined,
        }),
        host: null,
        ping: false,
      },
    ];
  const pingServers = details.servers.filter((server) => server.latencyTarget);
  return details.selection.map((server) => {
    const label = serverLabel(server);
    const host = new URL(server.url).host;
    return {
      id: server.id,
      label,
      host: host.toLowerCase() === label.toLowerCase() ? null : host,
      ping:
        details.selection.length > 1 &&
        pingServers.length === 1 &&
        pingServers[0].server.id === server.id,
    };
  });
}
