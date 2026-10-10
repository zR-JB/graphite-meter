// The diagnostic report: the state a run and its servers really hold, so a field added later is reported without
// listing it here, with what identifies a person or a deployment taken out.
import type { ServerEntry } from "../servers/catalog";

/** Keys never reported, at any depth: secrets, and the address the server saw for this browser. */
const WITHHELD = new Set([
  "credentials",
  "token",
  "authorization",
  "cookie",
  "clientIp",
]);

/** How a host reads without naming it. */
export function hostKind(host: string): string {
  const bare = host.replace(/^\[|\]$/g, "").toLowerCase();
  if (bare === "localhost" || bare.endsWith(".localhost")) return "loopback";
  if (bare.endsWith(".local")) return "local name";
  const v4 = /^(\d+)\.(\d+)\.\d+\.\d+$/.exec(bare);
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])];
    if (a === 127) return "loopback IPv4";
    const local =
      a === 10 ||
      (a === 172 && b >= 16 && b < 32) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254) ||
      (a === 100 && b >= 64 && b < 128);
    return local ? "private IPv4" : "public IPv4";
  }
  if (bare.includes(":")) {
    if (bare === "::1") return "loopback IPv6";
    return /^(f[cd]|fe[89ab])/.test(bare) ? "private IPv6" : "public IPv6";
  }
  return "name";
}

const URLISH = /\b(?:https?|wss?):\/\/[^\s"'<>]+/g;

/** Rewrites the report's values: servers become "server-N", the client address goes, secrets are dropped. */
function anonymizer(
  servers: readonly ServerEntry[],
  clientIps: string[],
  page: string,
) {
  const hosts = new Map<string, string>();
  const words = new Map<string, string>();
  const alias = (hostname: string) => {
    const key = hostname.toLowerCase();
    if (!hosts.has(key)) hosts.set(key, `server-${hosts.size + 1}`);
    return hosts.get(key)!;
  };
  for (const [index, server] of servers.entries()) {
    const hostname = URL.parse(server.url, page)?.hostname;
    if (hostname) words.set(hostname, alias(hostname));
    const label = `Server ${index + 1}`;
    if (server.name) words.set(server.name, label);
    if (server.location) words.set(server.location, `${label} location`);
    if (server.id !== "self") words.set(server.id, `server-${index + 1}`);
  }
  for (const ip of clientIps) words.set(ip, "client-address");
  // Whole words, longest first, so an id never rewrites part of another word or name.
  const known = [...words.keys()].sort((a, b) => b.length - a.length);
  const pattern = known.length
    ? new RegExp(
        `(?<![\\w.-])(?:${known.map((word) => word.replace(/[\\^$.*+?()[\]{}|/-]/g, "\\$&")).join("|")})(?![\\w-])`,
        "g",
      )
    : null;
  const scrub = (text: string) => {
    const out = text.replace(URLISH, (url) => {
      const parsed = URL.parse(url);
      if (!parsed) return "url";
      parsed.hostname = alias(parsed.hostname);
      return url.endsWith("/") ? parsed.href : parsed.href.replace(/\/$/, "");
    });
    return pattern ? out.replace(pattern, (word) => words.get(word)!) : out;
  };
  const walk = (value: unknown): unknown => {
    if (typeof value === "string") return scrub(value);
    if (Array.isArray(value)) return value.map(walk);
    if (value instanceof Map) return walk(Object.fromEntries(value));
    if (value && typeof value === "object")
      return Object.fromEntries(
        Object.entries(value)
          .filter(([key]) => !WITHHELD.has(key))
          .map(([key, item]) => [scrub(key), walk(item)]),
      );
    return value;
  };
  const kinds = () =>
    Object.fromEntries(
      [...hosts].map(([hostname, label]) => [label, hostKind(hostname)]),
    );
  return { walk, kinds };
}

export interface DiagnosticSources {
  /** The page's address, which a catalogue's relative server URLs resolve against. */
  page: string;
  /** Every server the report can mention: the catalogue's and the run's. */
  servers: readonly ServerEntry[];
  /** The addresses servers saw for this browser, scrubbed from free text too. */
  clientIps: string[];
  /** Plain state to report: build, environment, settings, servers' views and the run. */
  state: Record<string, unknown>;
}

export function diagnosticReport({
  page,
  servers,
  clientIps,
  state,
}: DiagnosticSources): string {
  const { walk, kinds } = anonymizer(servers, clientIps, page);
  const body = walk(state) as Record<string, unknown>;
  return JSON.stringify(
    {
      report: "Graphite Meter diagnostics",
      anonymized:
        "Servers, names and locations are numbered; this browser's address and credentials are left out.",
      hosts: kinds(),
      ...body,
    },
    null,
    2,
  );
}
