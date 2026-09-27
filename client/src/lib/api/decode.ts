/** Control documents are small; bound decoded bytes even without Content-Length. */
export const MAX_CONTROL_BYTES = 64 * 1024;
const MAX_TARGETS = 32;

export async function readJSONResponse(response: Response): Promise<unknown> {
  if (!response.body) throw new Error("empty control response");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_CONTROL_BYTES)
        throw new Error("control response too large");
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

export const isCount = (value: unknown): value is number =>
  Number.isSafeInteger(value) && (value as number) >= 0;

function record(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) throw new Error("expected control response object");
  return value;
}

function string(value: unknown, max: number, allowEmpty = false): string {
  if (
    typeof value !== "string" ||
    new TextEncoder().encode(value).byteLength > max ||
    (!allowEmpty && !value.length)
  )
    throw new Error("invalid control response string");
  return value;
}

// Controls and bidirectional overrides could disguise a server's displayed identity.
const UNSAFE_TEXT = /[\p{Cc}؜‎‏‪-‮⁦-⁩]/u;

export function displayText(value: unknown, max: number, allowEmpty = false) {
  const text = string(value, max, allowEmpty);
  if (UNSAFE_TEXT.test(text)) throw new Error("invalid control response text");
  return text;
}

export const safeDetail = (text: string, max: number) =>
  UNSAFE_TEXT.test(text) ? "" : text.slice(0, max);

const known = <T extends string>(
  value: unknown,
  values: readonly T[],
): value is T => values.includes(value as T);

/** A newer server's mechanism is skipped; a missing one refuses the document (api/preflight.forward.golden.json). */
function mechanism<T extends string>(
  value: unknown,
  values: readonly T[],
): T | null {
  if (known(value, values)) return value;
  if (typeof value === "string" && value) return null;
  throw new Error("unsupported control response value");
}

function member<T extends string>(value: unknown, values: readonly T[]): T {
  if (!known(value, values))
    throw new Error("unsupported control response value");
  return value;
}

const THROUGHPUT_TRANSPORTS = [
  "fetch-stream",
  "webtransport",
  "webtransport-datagram",
] as const;
const THROUGHPUT_PROTOCOLS = ["http1", "http2", "http3", "negotiated"] as const;
const LATENCY_TRANSPORTS = ["websocket", "webtransport"] as const;

function origin(value: unknown): string {
  const raw = string(value, 2048);
  if (raw === ".") return raw;
  const url = new URL(raw);
  if (
    !/^https?:\/\/[^@/?#\\\s]+$/.test(raw) ||
    url.username ||
    url.password ||
    !url.hostname ||
    (url.protocol !== "https:" && url.protocol !== "http:")
  )
    throw new Error("target baseUrl must be an HTTP(S) origin");
  return url.origin;
}

function targets(value: unknown): unknown[] {
  if (!Array.isArray(value) || value.length > MAX_TARGETS)
    throw new Error("invalid discovery target list");
  return value;
}

/** Discovery with independently selectable throughput and latency endpoints (api/preflight.schema.json). */
export type Preflight = ReturnType<typeof parsePreflight>;
export type ThroughputEndpoint =
  Preflight["capabilities"]["throughput"][number];
export type LatencyEndpoint = Preflight["capabilities"]["latency"][number];
/** Connection evidence from GET /probe on a selected target (api/probe.schema.json). */
export interface Probe {
  clientIp: string;
  clientIpVersion: 4 | 6;
  clientIpSource: "socket" | "forwarded";
  protocolNegotiated: "http/1.1" | "h2" | "h3";
  /** Admission-wrapped handlers holding slots, and the configured ceiling. */
  load?: { active: number; max: number };
}

export function parsePreflight(value: unknown) {
  const input = record(value);
  const server = record(input.server);
  const capabilities = record(input.capabilities);
  return {
    server: {
      name: displayText(server.name, 256, true),
      ...(server.location === undefined
        ? {}
        : { location: displayText(server.location, 256, true) }),
    },
    engineVersion: displayText(input.engineVersion, 256, true),
    generation: string(input.generation, 256),
    capabilities: {
      ...(capabilities.uploadCheckpoint === undefined
        ? {}
        : { uploadCheckpoint: capabilities.uploadCheckpoint === true }),
      throughput: targets(capabilities.throughput).flatMap((value) => {
        const target = record(value);
        const transport = mechanism(target.transport, THROUGHPUT_TRANSPORTS);
        const protocol = mechanism(target.protocol, THROUGHPUT_PROTOCOLS);
        return transport && protocol
          ? [{ baseUrl: origin(target.baseUrl), transport, protocol }]
          : [];
      }),
      latency: targets(capabilities.latency).flatMap((value) => {
        const target = record(value);
        const transport = mechanism(target.transport, LATENCY_TRANSPORTS);
        return transport
          ? [{ baseUrl: origin(target.baseUrl), transport }]
          : [];
      }),
    },
  };
}

export function parseProbe(value: unknown): Probe {
  const input = record(value);
  const clientIpVersion = input.clientIpVersion;
  if (clientIpVersion !== 4 && clientIpVersion !== 6)
    throw new Error("invalid probe IP version");
  let load: Probe["load"];
  if (input.load !== undefined) {
    const raw = record(input.load);
    if (!isCount(raw.active) || !isCount(raw.max) || raw.max < 1)
      throw new Error("invalid probe load");
    load = { active: raw.active, max: raw.max };
  }
  return {
    clientIp: string(input.clientIp, 64),
    clientIpVersion,
    clientIpSource: member(input.clientIpSource, ["socket", "forwarded"]),
    protocolNegotiated: member(input.protocolNegotiated, [
      "http/1.1",
      "h2",
      "h3",
    ]),
    ...(load ? { load } : {}),
  };
}

export function parseResponseToken(
  value: unknown,
  key: "token" | "uploadId",
): string {
  return string(record(value)[key], 8192);
}

/** Session lifetime values are milliseconds remaining when the response was produced. */
export function parseSessionLifetime(value: unknown): {
  remainingMs: number;
  maximumLifetimeMs: number;
} {
  const input = record(value);
  const { remainingMs, maximumLifetimeMs } = input;
  if (
    typeof remainingMs !== "number" ||
    !Number.isFinite(remainingMs) ||
    remainingMs < 0 ||
    typeof maximumLifetimeMs !== "number" ||
    !Number.isFinite(maximumLifetimeMs) ||
    maximumLifetimeMs <= 0 ||
    remainingMs > maximumLifetimeMs
  )
    throw new Error("invalid session lifetime");
  return { remainingMs, maximumLifetimeMs };
}

/** Only the account fields consumed by the browser cross into presentation. */
export function parseAccountSession(value: unknown): {
  name: string;
  provider: string;
  csrf: string;
} {
  const input = record(value);
  return {
    name: string(input.name, 8192, true),
    provider: string(input.provider, 8192),
    csrf: string(input.csrf, 8192),
  };
}

/** Mint expiry is an epoch-millisecond integer; authentication-off uses zero. */
export function parseWtToken(value: unknown): {
  token: string;
  expires: number;
} {
  const input = record(value);
  const token = string(input.token, 8192, true);
  const { expires } = input;
  if (!isCount(expires)) throw new Error("invalid WebTransport token expiry");
  return { token, expires };
}
