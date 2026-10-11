// The control documents the servers answer (api/discovery.md): small JSON bodies, read with a 64 KiB bound and
// decoded strictly before anything acts on them.
import { array, DecodeError, integer, object, oneOf, string, type Decoded, type Decoder } from "../lib/decode";

const MAX_BODY_BYTES = 64 * 1024;
const MAX_TARGETS = 32;
export const DEFAULT_STAGE_LIMIT_MS = 300_000;
export const MAX_STAGE_LIMIT_MS = 86_400_000;

/** One JSON document of at most 64 KiB of strict UTF-8, decoded bytes counted as they stream; the body is always
    released. */
export async function readJSON(response: Response): Promise<unknown> {
  if (!response.body) throw new DecodeError("the response has no body");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if ((size += value.byteLength) > MAX_BODY_BYTES) throw new DecodeError("the response is too large");
      chunks.push(value);
    }
  } finally {
    reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) bytes.set(chunk, (offset += chunk.byteLength) - chunk.byteLength);
  // Malformed text is the server's fault, never a network failure.
  try {
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new DecodeError("the response is not a JSON document");
  }
}

// Controls and bidirectional overrides could disguise a server's displayed identity.
const UNSAFE_TEXT = /[\p{Cc}؜‎‏‪-‮⁦-⁩]/u;

/** Text a view shows: bounded, and refused when it carries controls or direction overrides. */
export const displayText =
  (max: number, min = 0): Decoder<string> =>
  (value, path = "") => {
    const text = string(min, max)(value, path);
    if (UNSAFE_TEXT.test(text)) throw new DecodeError(`${path} must not contain control characters`);
    return text;
  };

/** A server's detail made safe for a saved result: empty when it carries controls, else at most `max` characters. */
export const safeDetail = (text: string, max = 256) => (UNSAFE_TEXT.test(text) ? "" : text.slice(0, max));

const ORIGIN = /^https?:\/\/[^@/?#\\\s]+$/;

/** An HTTP(S) origin as the browser writes it (default port dropped, host lower-cased, IDN in punycode). */
export function canonicalOrigin(text: string, path = ""): string {
  let url: URL | undefined;
  try {
    if (text.length <= 2048 && ORIGIN.test(text)) url = new URL(text);
  } catch {}
  if (!url?.hostname || url.username || url.password || url.port === "0")
    throw new DecodeError(`${path || "the address"} must be an HTTP(S) origin`);
  return url.origin;
}

/** "." (the origin that served discovery) or an HTTP(S) origin. */
const baseUrl: Decoder<string> = (value, path = "") => {
  const text = string(1, 2048)(value, path);
  return text === "." ? text : canonicalOrigin(text, path);
};

/** A mechanism the client knows, or null for a newer one the target list skips; absent or empty is malformed. */
const mechanism =
  <const T extends readonly string[]>(...known: T): Decoder<T[number] | null> =>
  (value, path = "") => {
    const text = string(1)(value, path);
    return known.includes(text) ? (text as T[number]) : null;
  };

/** At most 32 targets as sent; one naming an unknown mechanism is skipped before its address is checked. */
function targets<T extends Record<string, Decoder<string | null>>>(
  mechanisms: T,
): Decoder<({ baseUrl: string } & { [K in keyof T]: NonNullable<Decoded<T[K]>> })[]> {
  return (value, path = "") => {
    if (!Array.isArray(value)) throw new DecodeError(`${path} must be an array`);
    if (value.length > MAX_TARGETS) throw new DecodeError(`${path} must hold at most ${MAX_TARGETS} targets`);
    return value.flatMap((entry, index) => {
      const at = `${path}[${index}]`;
      const known = object(mechanisms)(entry, at);
      if (Object.values(known).some((each) => each === null)) return [];
      return [{ ...known, baseUrl: baseUrl((entry as Record<string, unknown>).baseUrl, `${at}.baseUrl`) }] as never;
    });
  };
}

export const preflight = object({
  server: object({ name: displayText(256) }, { location: displayText(256) }),
  engineVersion: displayText(256),
  generation: string(1, 256),
  capabilities: object(
    {
      throughput: targets({
        transport: mechanism("fetch-stream", "webtransport", "webtransport-datagram"),
        protocol: mechanism("http1", "http2", "http3", "negotiated"),
      }),
      latency: targets({ transport: mechanism("websocket", "webtransport") }),
    },
    {
      // Any value but true reads as false; a stage limit out of range refuses the document.
      uploadCheckpoint: (value: unknown) => value === true,
      maxStageMs: integer(1_000, MAX_STAGE_LIMIT_MS),
    },
  ),
});
export type Preflight = Decoded<typeof preflight>;

export const probe = object(
  {
    clientIp: string(1, 64),
    clientIpVersion: oneOf(4, 6),
    clientIpSource: oneOf("socket", "forwarded"),
    protocolNegotiated: oneOf("http/1.1", "h2", "h3"),
  },
  { load: object({ active: integer(), max: integer(1) }) },
);
export type Probe = Decoded<typeof probe>;

export const uploadSession = object({ uploadId: string(1, 8192) });

/** A receiver's counters; anything else is no checkpoint at all, not an error. */
export const checkpoint = object({ bytes: integer(), nanos: integer() });

/** A single-use socket ticket; authentication off answers an empty token. */
export const socketTicket = object({ token: string(0, 8192), expires: integer() });

/** How long a login or grant has left, within its maximum lifetime. */
const lifetime =
  <T extends object>(fields: Decoder<T>): Decoder<T & { remainingMs: number; maximumLifetimeMs: number }> =>
  (value, path = "") => {
    const decoded = object({ remainingMs: integer(), maximumLifetimeMs: integer(1) })(value, path);
    if (decoded.remainingMs > decoded.maximumLifetimeMs) throw new DecodeError(`${path} outlives its maximum lifetime`);
    return { ...fields(value, path), ...decoded };
  };

/** The page's own login (GET /auth/session). */
export const session = lifetime(object({ name: string(0, 8192), provider: string(1, 8192), csrf: string(1, 8192) }));

/** A peer's measurement grant (POST /auth/browser/token). */
export const grant = lifetime(
  object({
    token: (value: unknown, path = "") => {
      const token = string(43, 43)(value, path);
      if (!/^[A-Za-z0-9_-]{43}$/.test(token)) throw new DecodeError(`${path} must be a measurement grant`);
      return token;
    },
  }),
);

const serverId = (value: unknown, path = "") => {
  const id = displayText(64, 1)(value, path);
  if (!/^[A-Za-z0-9._-]+$/.test(id)) throw new DecodeError(`${path} must be a server ID`);
  return id;
};

const catalogOrigin = (value: unknown, path = "") => {
  const text = string(1, 2048)(value, path);
  if (/[*;]/.test(text)) throw new DecodeError(`${path} must be an HTTP(S) origin`);
  return canonicalOrigin(text.endsWith("/") ? text.slice(0, -1) : text, path);
};

export interface CatalogEntry {
  id: string;
  /** The canonical origin; the page's own for `self`. */
  url: string;
  /** The operator's fallback; discovery names the server once it answers. */
  name: string;
  location?: string;
  additionalOrigins: string[];
}
export interface Catalog {
  servers: CatalogEntry[];
  defaultSelection: string[];
}

/** The originating server's catalogue: `self` first, unique IDs and origins, 1–4 default IDs. Any violation refuses
    the whole document. */
export function catalog(value: unknown, pageOrigin: string): Catalog {
  const document = object({
    servers: array(
      object(
        { id: serverId, url: string(1, 2048), name: displayText(256) },
        {
          location: displayText(256),
          additionalOrigins: array(catalogOrigin, 32),
        },
      ),
      32,
      1,
    ),
    defaultSelection: array(serverId, 4, 1),
  })(value, "catalog");
  const ids = new Set<string>();
  const origins = new Set<string>();
  const servers = document.servers.map((entry, index): CatalogEntry => {
    if ((index === 0) !== (entry.id === "self") || ids.has(entry.id))
      throw new DecodeError("server IDs must be unique, with self first");
    const url = entry.url === "." && index === 0 ? canonicalOrigin(pageOrigin) : catalogOrigin(entry.url, "url");
    if (origins.has(url)) throw new DecodeError("server origins must be unique");
    ids.add(entry.id);
    origins.add(url);
    return {
      id: entry.id,
      url,
      name: entry.name || entry.id,
      ...(entry.location !== undefined && { location: entry.location }),
      additionalOrigins: entry.additionalOrigins ?? [],
    };
  });
  const selection = document.defaultSelection;
  if (new Set(selection).size !== selection.length || selection.some((id) => !ids.has(id)))
    throw new DecodeError("the default selection must name one to four catalogue servers");
  return { servers, defaultSelection: selection };
}
