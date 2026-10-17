// The diagnostic report carries the state the page really holds, so a field added later is reported without being
// listed here. It takes out what identifies a person or a deployment.

/** These keys are never reported at any depth. They hold secrets and the address the server saw for this browser. */
const WITHHELD = new Set(["credentials", "token", "authorization", "cookie", "clientIp"]);
const URLISH = /\b(?:https?|wss?):\/\/[^\s"'<>]+/g;

/** Describes a host without naming it. */
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

interface ReportSources {
  /** Relative server URLs resolve against this page address. */
  page: string;
  /** Holds every server the report can mention, whether from the catalog, from discovery's naming or from the run. */
  servers: readonly { id: string; url: string; name: string; location?: string }[];
  /** Holds the addresses servers saw for this browser, which are scrubbed from free text too. */
  clientIps: readonly string[];
  /** Holds plain state, from the build, environment and settings to the servers' views and the run. */
  state: Record<string, unknown>;
}

/** Renders pretty JSON in which servers read "server-N", their names "Server N" and this browser's address
    "client-address", with secrets gone. */
export function diagnosticReport({ page, servers, clientIps, state }: ReportSources): string {
  const hosts = new Map<string, string>();
  const words = new Map<string, string>();
  const alias = (hostname: string) => {
    const key = hostname.toLowerCase();
    if (!hosts.has(key)) hosts.set(key, `server-${hosts.size + 1}`);
    return hosts.get(key)!;
  };
  // A server listed twice (by the catalog and as discovery names it) keeps its number.
  const ids = [...new Set(servers.map(({ id }) => id))];
  for (const server of servers) {
    const number = ids.indexOf(server.id) + 1;
    const hostname = URL.parse(server.url, page)?.hostname;
    if (hostname) words.set(hostname, alias(hostname));
    if (server.name) words.set(server.name, `Server ${number}`);
    if (server.location) words.set(server.location, `Server ${number} location`);
    if (server.id !== "self") words.set(server.id, `server-${number}`);
  }
  for (const ip of clientIps) words.set(ip, "client-address");
  // The pattern matches whole words, longest first, so an ID never rewrites part of another word or name.
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
  const body = walk(state) as Record<string, unknown>;
  return JSON.stringify(
    {
      report: "Graphite Meter diagnostics",
      anonymized: "Servers, names and locations are numbered; this browser's address and credentials are left out.",
      hosts: Object.fromEntries([...hosts].map(([hostname, label]) => [label, hostKind(hostname)])),
      ...body,
    },
    null,
    2,
  );
}
