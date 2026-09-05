import { expect, test } from "bun:test";
import {
  MAX_CONTROL_BYTES,
  parsePreflight,
  parseProbe,
  parseResponseToken,
  parseSessionLifetime,
  parseAccountSession,
  parseWtToken,
  readJSONResponse,
} from "./decode";

const discovery = () => ({
  server: { name: "test" },
  engineVersion: "dev",
  generation: "a",
  capabilities: {
    throughput: [
      { baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" },
    ],
    latency: [{ baseUrl: "https://[::1]:7247", transport: "websocket" }],
  },
});

test("discovery preserves independent listeners, self origin, and explicit transports", () => {
  const result = parsePreflight(discovery());
  expect(result.capabilities.throughput[0]).toEqual({
    baseUrl: ".",
    protocol: "negotiated",
    transport: "fetch-stream",
  });
  expect(result.capabilities.latency[0]).toEqual({
    baseUrl: "https://[::1]:7247",
    transport: "websocket",
  });
});

test("discovery rejects malformed origins before target construction", () => {
  for (const baseUrl of [
    "https://u:p@example.com",
    "https://@example.com",
    "https://example.com/",
    "https://example.com/path",
    "https://example.com?",
    "https://example.com#",
    "https://example.com\\path",
    "//example.com",
    "ftp://example.com",
    " https://example.com",
    "https://example.com:99999",
  ]) {
    const value = discovery();
    value.capabilities.throughput[0]!.baseUrl = baseUrl;
    expect(() => parsePreflight(value)).toThrow();
  }
});

test("discovery bounds lists and metadata and rejects unknown protocols", () => {
  for (const value of [
    null,
    [],
    {},
    { ...discovery(), generation: "" },
    { ...discovery(), generation: "x".repeat(257) },
    {
      ...discovery(),
      capabilities: {
        throughput: Array(33).fill({
          baseUrl: ".",
          transport: "fetch-stream",
          protocol: "http1",
        }),
        latency: [],
      },
    },
    {
      ...discovery(),
      capabilities: {
        throughput: [
          { baseUrl: ".", transport: "fetch-stream", protocol: "http4" },
        ],
        latency: [],
      },
    },
  ])
    expect(() => parsePreflight(value)).toThrow();
});

test("probe validates evidence and occupancy", () => {
  const value = {
    clientIp: "127.0.0.1",
    clientIpVersion: 4,
    clientIpSource: "socket",
    protocolNegotiated: "h2",
  } as const;
  expect(parseProbe(value)).toEqual(value);
  for (const change of [
    { clientIpVersion: 5 },
    { clientIpSource: "unknown" },
    { protocolNegotiated: "quic" },
    { load: { active: -1, max: 2 } },
    { load: { active: 1.5, max: 2 } },
    { load: { active: 0, max: 0 } },
  ])
    expect(() => parseProbe({ ...value, ...change })).toThrow();
  expect(() => parseResponseToken({ token: "" }, "token")).toThrow();
  expect(() =>
    parseResponseToken({ uploadId: "x".repeat(8193) }, "uploadId"),
  ).toThrow();
});

test("control reads stop and cancel an oversized chunked body", async () => {
  let cancelled = false;
  let reads = 0;
  const body = new ReadableStream<Uint8Array>({
    pull(controller) {
      reads++;
      controller.enqueue(new Uint8Array(16384));
    },
    cancel() {
      cancelled = true;
    },
  });
  await expect(readJSONResponse(new Response(body))).rejects.toThrow(
    "too large",
  );
  expect(cancelled).toBe(true);
  expect(reads).toBeLessThanOrEqual(6);
});

test("control reads reject oversize, invalid UTF-8, and trailing documents", async () => {
  expect(
    await readJSONResponse(
      new Response('"' + "a".repeat(MAX_CONTROL_BYTES - 2) + '"'),
    ),
  ).toHaveLength(MAX_CONTROL_BYTES - 2);
  for (const body of [
    '"' + "a".repeat(MAX_CONTROL_BYTES - 1) + '"',
    "{} {}",
    new Uint8Array([34, 255, 34]),
  ])
    await expect(readJSONResponse(new Response(body))).rejects.toThrow();
});

test("auth documents validate lifetimes, account fields, and required finite mint expiry", () => {
  expect(
    parseSessionLifetime({ remainingMs: 0, maximumLifetimeMs: 1 }),
  ).toEqual({ remainingMs: 0, maximumLifetimeMs: 1 });
  for (const value of [
    null,
    [],
    { remainingMs: 2, maximumLifetimeMs: 1 },
    { remainingMs: 0, maximumLifetimeMs: Infinity },
  ])
    expect(() => parseSessionLifetime(value)).toThrow();
  const account = { name: "", provider: "local", csrf: "token" };
  expect(
    parseAccountSession({ ...account, expires: "unused", extra: "unused" }),
  ).toEqual(account);
  for (const value of [
    null,
    [],
    { ...account, csrf: "" },
    { ...account, name: "a".repeat(8193) },
  ])
    expect(() => parseAccountSession(value)).toThrow();
  expect(() => parseWtToken({ token: "gmw_old" })).toThrow();
  expect(parseWtToken({ token: "", expires: 0 })).toEqual({
    token: "",
    expires: 0,
  });
  expect(
    parseWtToken({ token: "gmw_new", expires: 1_800_000_000_000 }),
  ).toEqual({ token: "gmw_new", expires: 1_800_000_000_000 });
  for (const expires of [
    null,
    "soon",
    -1,
    1.5,
    Infinity,
    NaN,
    Number.MAX_SAFE_INTEGER + 1,
  ])
    expect(() => parseWtToken({ token: "gmw_invalid", expires })).toThrow();
});

test("discovery requires an explicit supported transport on every target", () => {
  for (const role of ["throughput", "latency"] as const) {
    for (const transport of [undefined, null, "", "udp"]) {
      const value = discovery();
      Object.assign(value.capabilities[role][0]!, { transport });
      expect(() => parsePreflight(value)).toThrow();
    }
  }
});
