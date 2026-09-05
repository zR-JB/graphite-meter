import type { Preflight } from "./preflight";
import type { Probe } from "./probe";

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

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("expected control response object");
  return value as Record<string, unknown>;
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

function member<T extends string>(value: unknown, values: readonly T[]): T {
  if (typeof value !== "string" || !values.includes(value as T))
    throw new Error("unsupported control response value");
  return value as T;
}

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

/** Missing transport is the supported pre-transport wire default. */
export function parsePreflight(value: unknown): Preflight {
  const input = record(value);
  const server = record(input.server);
  const capabilities = record(input.capabilities);
  return {
    server: {
      name: string(server.name, 256, true),
      ...(server.location === undefined
        ? {}
        : { location: string(server.location, 256, true) }),
    },
    engineVersion: string(input.engineVersion, 256, true),
    generation: string(input.generation, 256),
    capabilities: {
      throughput: targets(capabilities.throughput).map((value) => {
        const target = record(value);
        return {
          baseUrl: origin(target.baseUrl),
          transport: member(
            target.transport === undefined ? "fetch-stream" : target.transport,
            ["fetch-stream", "webtransport", "webtransport-datagram"],
          ),
          protocol: member(target.protocol, [
            "http1",
            "http2",
            "http3",
            "negotiated",
          ]),
        };
      }),
      latency: targets(capabilities.latency).map((value) => {
        const target = record(value);
        return {
          baseUrl: origin(target.baseUrl),
          transport: member(
            target.transport === undefined ? "websocket" : target.transport,
            ["websocket", "webtransport"],
          ),
        };
      }),
    },
  };
}

export function parseProbe(value: unknown): Probe {
  const input = record(value);
  if (input.clientIpVersion !== 4 && input.clientIpVersion !== 6)
    throw new Error("invalid probe IP version");
  let load: Probe["load"];
  if (input.load !== undefined) {
    const raw = record(input.load);
    if (
      !Number.isSafeInteger(raw.active) ||
      !Number.isSafeInteger(raw.max) ||
      (raw.active as number) < 0 ||
      (raw.max as number) < 1
    )
      throw new Error("invalid probe load");
    load = { active: raw.active as number, max: raw.max as number };
  }
  return {
    clientIp: string(input.clientIp, 64),
    clientIpVersion: input.clientIpVersion,
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
