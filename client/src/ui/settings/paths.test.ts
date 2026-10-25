import { expect, test } from "bun:test";
import { DEFAULT_CONFIG } from "../../app/preferences";
import type { Readiness, ServerView } from "../../engine/net/connection";
import type { PageConfig } from "../../engine/net/page";
import { classify } from "../../engine/net/paths";
import type { Discovery, Role } from "../../engine/types";
import type { Preflight } from "../../protocol/documents";
import { choices, describePath, roleLine } from "./paths";

type Browser = Pick<PageConfig, "origin" | "secure" | "webTransport">;
const PAGE: Browser = { origin: "https://ui.example", secure: true, webTransport: "no-api" };

function offer(
  origin: string,
  throughput: object[],
  latency: object[] = [],
  page = PAGE,
): Discovery {
  const capabilities = { throughput, latency } as Preflight["capabilities"];
  return {
    generation: "g1",
    engineVersion: "",
    server: { name: "" },
    maxStageMs: 300_000,
    origin,
    protocol: null,
    readAt: 0,
    requestMs: 0,
    ...classify(capabilities, origin, page),
  };
}

const role = (selection = "auto") => ({
  selection,
  state: "unchecked" as const,
  path: null,
  message: null,
});
function view(
  id: string,
  discovery: Discovery | null,
  readiness: Readiness = "verified",
): ServerView {
  return {
    server: { id, url: `https://${id}.example`, name: id.toUpperCase() },
    discovery,
    readiness,
    message: null,
    blocked: null,
    throughput: role(),
    latency: role(),
    ready: false,
    metadataChecking: false,
    idle: false,
    connectivity: null,
  };
}
const configFor = (key: Role, selected: string, datagrams = false) => ({
  ...DEFAULT_CONFIG,
  experimentalDatagramThroughput: datagrams,
  transports: {
    ...DEFAULT_CONFIG.transports,
    [key === "throughput" ? "throughputTarget" : "latencyTarget"]: selected,
  },
});
const byValue = (list: ReturnType<typeof choices>) =>
  Object.fromEntries(list.map((each) => [each.value, each]));

const a = "https://a.example";
const b = "https://b.example";
const views = (readinessB: Readiness = "verified") =>
  new Map([
    [
      "a",
      view(
        "a",
        offer(
          a,
          [{ baseUrl: ".", transport: "fetch-stream", protocol: "http1" }],
          [{ baseUrl: ".", transport: "websocket" }],
        ),
      ),
    ],
    [
      "b",
      view(
        "b",
        offer(
          b,
          [{ baseUrl: ".", transport: "fetch-stream", protocol: "http2" }],
          [{ baseUrl: ".", transport: "websocket" }],
        ),
        readinessB,
      ),
    ],
  ]);
const both = [...views().values()].map(({ server }) => server);

test("with several servers a group is available only where every answering server can run it", () => {
  const throughput = byValue(
    choices("throughput", both, views(), configFor("throughput", "auto"), PAGE),
  );
  expect(throughput.auto?.unavailable).toBeNull();
  expect(throughput["protocol:http1"]?.unavailable).toContain("B");
  expect(throughput["protocol:http2"]?.unavailable).toContain("A");
  expect(throughput["transport:webtransport-datagram"]).toBeUndefined();
  const latency = byValue(choices("latency", both, views(), configFor("latency", "auto"), PAGE));
  expect(latency["transport:websocket"]?.unavailable).toBeNull();
  // A failed server's last discovery says nothing about today.
  const failed = byValue(
    choices("throughput", both, views("failed"), configFor("throughput", "auto"), PAGE),
  );
  expect(failed["protocol:http1"]?.unavailable).toBeNull();
  // An exact origin is kept visible among the groups.
  const exact = choices("throughput", both, views(), configFor("throughput", a), PAGE);
  expect(exact.at(-1)).toMatchObject({ value: a, label: "Selected origin" });
});

test("one server offers its own mechanisms, each with why it cannot be chosen", () => {
  const discovery = offer(a, [
    { baseUrl: ".", transport: "fetch-stream", protocol: "http1" },
    { baseUrl: ".", transport: "webtransport" },
    { baseUrl: ".", transport: "webtransport-datagram" },
  ]);
  const one = view("a", discovery);
  const offered = choices(
    "throughput",
    [one.server],
    new Map([["a", one]]),
    configFor("throughput", "auto"),
    PAGE,
  );
  expect(offered.map(({ label, unavailable }) => [label, unavailable !== null])).toEqual([
    ["Automatic", false],
    ["HTTP/1.1 (TLS)", false],
    ["WebTransport streams (TLS)", true],
  ]);
  const datagrams = choices(
    "throughput",
    [one.server],
    new Map([["a", one]]),
    configFor("throughput", "auto", true),
    PAGE,
  );
  // A saved exact path stays listed before its server answers, so the picker never shows no choice.
  const exact = "https://a.example::wt";
  const waiting = view("a", null, "checking");
  expect(
    choices(
      "throughput",
      [waiting.server],
      new Map([["a", waiting]]),
      configFor("throughput", exact),
      PAGE,
    ).at(-1),
  ).toMatchObject({
    value: exact,
    label: "Selected origin",
    unavailable: "Checking server transports…",
  });
  expect(datagrams.at(-1)).toMatchObject({
    value: `${a}::wtdg`,
    label: "WebTransport datagrams (TLS)",
  });
  const unchecked = choices(
    "throughput",
    [one.server],
    new Map(),
    configFor("throughput", "auto"),
    PAGE,
  );
  expect(unchecked.map(({ value, unavailable }) => [value, unavailable !== null])).toEqual([
    ["auto", true],
  ]);
});

test("a choice the browser's policy excludes names the remedy", () => {
  const page: Browser = {
    origin: "http://ui.example",
    secure: false,
    webTransport: "insecure-page",
  };
  const origin = "http://[::1]:7246";
  const one = view(
    "ipv6",
    offer(origin, [{ baseUrl: ".", transport: "fetch-stream", protocol: "http1" }], [], page),
  );
  const offered = byValue(
    choices(
      "throughput",
      [one.server],
      new Map([["ipv6", one]]),
      configFor("throughput", "auto"),
      page,
    ),
  );
  expect(offered.auto?.unavailable).toBe(one.discovery!.throughput.get(origin)!.blocked);
  expect(offered[origin]?.unavailable).toBe(offered.auto!.unavailable);
});

test("a path reads as its mechanism over its carrier", () => {
  const discovery = offer(a, [{ baseUrl: ".", transport: "fetch-stream", protocol: "http1" }]);
  const cases: [Parameters<typeof describePath>[1], Parameters<typeof describePath>[2], string][] =
    [
      [
        "throughput",
        { transport: "fetch-stream", origin: a, tls: true, protocol: "http2" },
        "Fetch streams over HTTP/2 (TLS)",
      ],
      [
        "throughput",
        { transport: "webtransport", origin: a, tls: true },
        "WebTransport streams over HTTP/3 (TLS)",
      ],
      [
        "throughput",
        { transport: "webtransport-datagram", origin: a, tls: true },
        "WebTransport datagrams over HTTP/3 (TLS)",
      ],
      [
        "latency",
        { transport: "webtransport", origin: a, tls: true },
        "WebTransport datagrams over HTTP/3 (TLS)",
      ],
      [
        "latency",
        { transport: "websocket", origin: a, tls: true },
        "WebSocket over HTTP/1.1 (TLS)",
      ],
      ["latency", { transport: "websocket", origin: b, tls: true }, "WebSocket (TLS)"],
    ];
  for (const [path, target, summary] of cases)
    expect(describePath(discovery, path, target).summary).toBe(summary);
});

test("a line on a fallback says what Automatic used and why, in place of a plain Ready", () => {
  const discovery = offer(a, [], [{ baseUrl: ".", transport: "websocket" }]);
  const target = {
    id: a,
    origin: a,
    transport: "websocket",
    protocol: "http1",
    tls: true,
  } as const;
  const fellBack = view("a", discovery);
  fellBack.latency = {
    selection: "auto",
    state: "verified",
    message: null,
    path: {
      target,
      verifiedFor: { selection: "auto", head: `${a}::wt`, fallback: "WebTransport did not open" },
    } as unknown as ServerView["latency"]["path"],
  };
  expect(roleLine("latency", [fellBack], false, PAGE)).toEqual({
    state: "verified",
    status: { label: "Ready on a fallback", tone: "warn" },
    summary: "Automatic used WebSocket (TLS) for latency: WebTransport did not open",
  });
  const plain = view("b", discovery);
  plain.latency = {
    ...fellBack.latency,
    path: { ...fellBack.latency.path!, verifiedFor: { selection: "auto", head: a } },
  };
  expect(roleLine("latency", [fellBack, plain], true, PAGE)?.summary).toBe(
    "1 of 2 servers on a fallback",
  );
});
