// What Details says of one server: its load, the HTTP version each side saw, what it advertises, and the issues it
// hit in the shown run.
import { protocolFromNextHop } from "../../engine/net/paths";
import type { MultiServerResult } from "../../engine/run/result";
import type { Discovery, OriginTargets, Probe, Role } from "../../engine/types";
import { HTTP_VERSION, POPULATION, reasonLabel, STAGE, transportLabel } from "../../view/words";

/** Past this share of its slots, concurrent tests share the bandwidth this run measures. */
const BUSY_SHARE = 0.5;

/** Occupancy as the server reported it when the path was checked; nothing without a slot limit. */
export function loadSummary(load: Probe["load"]): string | null {
  if (!load || load.max <= 0) return null;
  const slots = `${load.active} of ${load.max} slots`;
  return load.active / load.max > BUSY_SHARE ? `${slots}, server busy: results may be affected` : slots;
}

/** An HTTP version as a browser's next hop or a server's report names it; an unknown name reads as it came. */
export function httpVersion(name: string): string {
  const known = protocolFromNextHop(name) ?? name;
  return Object.hasOwn(HTTP_VERSION, known) ? HTTP_VERSION[known as keyof typeof HTTP_VERSION] : name;
}

/** Each side's own observation, never merged: "Browser HTTP/1.1, server HTTP/2"; `unknown` until either reports. */
export function evidence(browser: string | null, server: string | null, unknown: string): string {
  const seen = [browser && `Browser ${httpVersion(browser)}`, server && `server ${httpVersion(server)}`].filter(
    Boolean,
  );
  const text = seen.join(", ");
  return text ? text[0]!.toUpperCase() + text.slice(1) : unknown;
}

const origins = (discovery: Discovery, role: Role) => [
  ...(
    discovery[role] as ReadonlyMap<string, OriginTargets<{ transport: string; protocol: string; tls: boolean }>>
  ).values(),
];

/** Every HTTP version and security the server's fetch targets advertise, on every origin; null before discovery. */
export function httpPaths(discovery: Discovery | null): string[] | null {
  if (!discovery) return null;
  const targets = origins(discovery, "throughput").flatMap((entry) => entry.targets);
  return [
    ...new Set(
      targets
        .filter((target) => target.transport === "fetch-stream")
        .map((target) => `${httpVersion(target.protocol)} (${target.tls ? "TLS" : "clear"})`),
    ),
  ];
}

/** The mechanisms the server advertises for `role`, and whether this page cannot reach some of its origins; null
    before discovery. */
export function capabilities(discovery: Discovery | null, role: Role): string[] | null {
  if (!discovery) return null;
  const entries = origins(discovery, role);
  const names = new Set(
    entries.flatMap((entry) =>
      entry.targets.map((target) => transportLabel(target.transport, role) ?? target.transport),
    ),
  );
  if (entries.some((entry) => entry.blocked)) names.add("Some clear origins blocked by this page");
  return [...names];
}

/** One line per reason a server failed in the run: the stages it ended, a transfer's latency named as its
    population, and why. */
export function serverIssues(details: MultiServerResult, id: string): { stages: string; reason: string }[] {
  const lines = new Map<string, string[]>();
  for (const failure of details.failures) {
    if (failure.serverId !== id) continue;
    const stages = lines.get(failure.reason) ?? [];
    lines.set(failure.reason, stages);
    stages.push(
      failure.scope === "latency" && failure.stage !== "latency"
        ? POPULATION[failure.stage]
        : STAGE[failure.stage].label,
    );
  }
  return [...lines].map(([reason, stages]) => ({ stages: stages.join(", "), reason: reasonLabel(reason) }));
}
