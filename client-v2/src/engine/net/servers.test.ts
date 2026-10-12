import { afterEach, expect, test } from "bun:test";
import { signal } from "../../lib/signal";
import { catalog } from "../../protocol/documents";
import type { TestConfig } from "../types";
import { CONFIG, FakeBuses, fakeNet, h, MARKER, page, preflight, PROBE, ws, type Answer } from "./fakes";
import { createPageSession } from "./page";
import { createServers, reconcile } from "./servers";

let net: ReturnType<typeof fakeNet> | undefined;
const cleanups: (() => void)[] = [];
afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
  net?.restore();
});

const HOME = { servers: [{ id: "self", url: ".", name: "Home" }], defaultSelection: ["self"] };

/** The page's servers on the fake network, whose catalogue and server answer as `route` says, once loaded. */
async function load(
  route: (url: URL, init: RequestInit) => Answer,
  transports: Partial<TestConfig["transports"]> = {},
) {
  net = fakeNet(route);
  const config = signal<TestConfig>({ ...CONFIG, transports: { ...CONFIG.transports, ...transports } });
  const active = signal(true);
  const online = signal(true);
  const servers = createServers({
    page,
    session: createPageSession(page, net.now),
    config,
    setTransports: (next) => config.set({ ...config.peek(), transports: next }),
    active,
    online,
    latencyPin: signal(null),
    openProbeChannel: new FakeBuses().open,
    idleLatency() {},
    now: net.now,
  });
  cleanups.push(servers.dispose);
  await servers.load();
  await net.advance(1_000);
  /** Starts the run's preparation: its outcome once settled, or the name of the error that ended it. */
  const prepare = () =>
    servers.prepare(new AbortController().signal).then(
      ({ servers, dropped }) => ({ servers: servers.map(({ server }) => server.id), dropped }),
      (error: Error) => error.name,
    );
  const readiness = (id = "self") => servers.views.peek().get(id)?.readiness;
  return { servers, config, active, online, prepare, readiness };
}

test("Start runs on what its checks verified however long they take, and never fails without a message", async () => {
  let probeMs = 0;
  const { servers, active, online, prepare, readiness } = await load((url) =>
    url.pathname === "/servers"
      ? { body: HOME }
      : url.pathname === "/preflight"
        ? { body: preflight([h(".", "http1")], [ws(".")]) }
        : { body: PROBE, hop: "http/1.1", afterMs: probeMs },
  );
  expect(readiness()).toBe("verified");
  // A run holds the checks; at 119 s it fails on latency, and Start follows while that path is checked for 1.5 s.
  active.set(false);
  await net!.advance(118_000);
  servers.recheck({ serverId: "self", scope: "latency", reason: "timeout", message: "Latency probes timed out" });
  await net!.advance(900);
  probeMs = 1_500;
  const late = prepare();
  await net!.advance(2_000);
  expect(await late).toEqual({ servers: ["self"], dropped: [] });
  // Evidence a network change takes while Start checks drops the server with words, never silently.
  probeMs = 0;
  servers.recheck({ serverId: "self", scope: "latency", reason: "timeout", message: "Latency probes timed out" });
  const blip = prepare();
  await net!.advance(200);
  online.set(false);
  online.set(true);
  await net!.advance(1_000);
  expect(await blip).toEqual({
    servers: [],
    dropped: [
      {
        server: { id: "self", url: page.origin, name: "Meter" },
        reason: "preparation-failed",
        message: "Connection check failed",
      },
    ],
  });
});

test("a catalogue reload during Start supersedes it rather than running on closed connections", async () => {
  let preflightMs = 0;
  const { servers, active, prepare } = await load((url) =>
    url.pathname === "/servers"
      ? { body: HOME }
      : url.pathname === "/preflight"
        ? { body: preflight([h(".", "http1")], [ws(".")]), afterMs: preflightMs }
        : { body: PROBE, hop: "http/1.1" },
  );
  active.set(false);
  preflightMs = 1_000;
  const pending = prepare();
  await net!.advance(500);
  await servers.load();
  await net!.advance(1_000);
  expect(await pending).toBe("AbortError");
});

test("a reloaded catalogue that no longer allows the chosen origin keeps the protocol it named", async () => {
  let additionalOrigins = ["https://fast.example"];
  const { servers, config } = await load(
    (url) =>
      url.pathname === "/servers"
        ? { body: { ...HOME, servers: [{ ...HOME.servers[0], additionalOrigins }] } }
        : url.pathname === "/preflight"
          ? { body: preflight([h(".", "http1"), h("https://fast.example", "http2")], [ws(".")]) }
          : { body: PROBE, hop: url.hostname === "fast.example" ? "h2" : "http/1.1" },
    { throughputTarget: "https://fast.example" },
  );
  expect(servers.views.peek().get("self")?.throughput.path?.target.id).toBe("https://fast.example");
  additionalOrigins = [];
  await servers.load();
  expect(config.peek().transports).toEqual({ throughputTarget: "protocol:http2", latencyTarget: "auto" });
});

test("an approved peer checks its grant at once, even while the approving window hides this page", async () => {
  const peer = "https://peer.example";
  const catalogue = {
    servers: [...HOME.servers, { id: "peer", url: peer, name: "Peer" }],
    defaultSelection: ["self", "peer"],
  };
  const grant = { token: "t".repeat(43), remainingMs: 3_600_000, maximumLifetimeMs: 3_600_000 };
  const { servers, active, readiness } = await load((url, init) => {
    if (url.pathname === "/servers") return { body: catalogue };
    if (url.pathname === "/auth/browser/token") return { body: grant };
    if (url.origin === peer && !new Headers(init.headers).has("Authorization")) return { status: 403, headers: MARKER };
    return url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")], [ws(".")]) }
      : { body: PROBE, hop: "http/1.1" };
  });
  expect(readiness("peer")).toBe("sign-in");
  servers.signIn("peer");
  active.set(false);
  await net!.advance(2_000);
  expect({ approval: servers.approval.peek(), readiness: readiness("peer") }).toEqual({
    approval: { state: "idle" },
    readiness: "verified",
  });
});

test("a catalogue that failed to load is read again as the network or the page comes back", async () => {
  let reachable = false;
  const { servers, online } = await load((url) =>
    !reachable
      ? "unreachable"
      : url.pathname === "/servers"
        ? { body: HOME }
        : url.pathname === "/preflight"
          ? { body: preflight([h(".", "http1")]) }
          : { body: PROBE, hop: "http/1.1" },
  );
  expect(servers.loadError.peek()).toBe("Could not load the server catalog");
  online.set(false);
  reachable = true;
  online.set(true);
  await net!.advance(1_000);
  expect({ error: servers.loadError.peek(), selected: servers.selected.peek() }).toEqual({
    error: "",
    selected: ["self"],
  });
  // A loaded catalogue is never read again on its own.
  const sent = net!.sent("/servers").length;
  online.set(false);
  online.set(true);
  servers.resume();
  await net!.advance(1_000);
  expect(net!.sent("/servers")).toHaveLength(sent);

  reachable = false;
  await servers.load();
  expect(servers.loadError.peek()).toBe("Could not load the server catalog");
  reachable = true;
  servers.resume();
  await net!.advance(1_000);
  expect(servers.loadError.peek()).toBe("");
});

test("a failed approval keeps no link to its spent challenge, only the words of why", async () => {
  const peer = "https://peer.example";
  const catalogue = {
    servers: [...HOME.servers, { id: "peer", url: peer, name: "Peer" }],
    defaultSelection: ["self", "peer"],
  };
  const { servers } = await load((url, init) => {
    if (url.pathname === "/servers") return { body: catalogue };
    if (url.pathname === "/auth/browser/token") return { status: 410 };
    if (url.origin === peer && !new Headers(init.headers).has("Authorization")) return { status: 403, headers: MARKER };
    return url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")], [ws(".")]) }
      : { body: PROBE, hop: "http/1.1" };
  });
  servers.signIn("peer");
  await net!.advance(1_000);
  expect({ approval: servers.approval.peek(), message: servers.views.peek().get("peer")?.message }).toEqual({
    approval: { state: "failed", id: "peer" },
    message: "The approval's answer was lost on the way. Sign in again.",
  });
});

test("a saved choice binds IDs to origins, prototype-named IDs included", () => {
  const known = catalog(
    {
      defaultSelection: ["toString", "self"],
      servers: [
        { id: "self", url: ".", name: "Home" },
        { id: "constructor", url: "https://a.example", name: "A" },
        { id: "__proto__", url: "https://b.example", name: "B" },
        { id: "toString", url: "https://c.example", name: "C" },
      ],
    },
    "https://meter.example",
  );
  const defaults = { ids: ["self", "toString"], unresolved: [] };
  const row = (id: string, url: string) => ({ id, url });
  for (const [saved, expected] of [
    [null, defaults],
    ["self", defaults],
    [[], defaults],
    [
      [row("self", "https://meter.example"), row("constructor", "https://a.example")],
      { ids: ["self", "constructor"], unresolved: [] },
    ],
    [
      [row("toString", "https://c.example"), row("constructor", "https://a.example")],
      { ids: ["constructor", "toString"], unresolved: [] },
    ],
    [JSON.parse('[{"id":"__proto__","url":"https://b.example"}]'), { ids: ["__proto__"], unresolved: [] }],
    [
      [row("constructor", "https://moved.example")],
      { ids: [], unresolved: [row("constructor", "https://moved.example")] },
    ],
    [[row("valueOf", "https://a.example")], { ids: [], unresolved: [row("valueOf", "https://a.example")] }],
    [[row("self", "https://meter.example"), row("self", "https://meter.example")], defaults],
    [[row("self", "https://meter.example"), "self"], defaults],
    [[row("self", "https://meter.example/")], defaults],
    [[row("", "https://meter.example")], defaults],
    [["a", "b", "c", "d", "e"].map((id) => row(id, "https://a.example")), defaults],
  ] as const)
    expect<unknown>(reconcile(known, saved), JSON.stringify(saved)).toEqual(expected);
});

test("a peer grant that ends before the test asks to renew its login, and the next Start checks again", async () => {
  const peer = "https://peer.example";
  const catalogue = {
    servers: [...HOME.servers, { id: "peer", url: peer, name: "Peer" }],
    defaultSelection: ["self", "peer"],
  };
  const grant = { token: "t".repeat(43), remainingMs: 3_600_000, maximumLifetimeMs: 28_800_000 };
  const { servers, readiness } = await load((url, init) => {
    if (url.pathname === "/servers") return { body: catalogue };
    if (url.pathname === "/auth/browser/token") return { body: grant };
    if (url.origin === peer && !new Headers(init.headers).has("Authorization")) return { status: 403, headers: MARKER };
    return url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")], [ws(".")]) }
      : { body: PROBE, hop: "http/1.1" };
  });
  servers.signIn("peer");
  await net!.advance(2_000);
  const authorize = (ms: number) =>
    servers.authorize(ms, new AbortController().signal).then(
      () => "covered",
      (error: Error) => error.message,
    );
  // Another approval would end with the same login, so the words point at renewing it, and the peer stays checked.
  expect(await authorize(2 * 3_600_000)).toBe(
    "Meter: its sign-in ends before this test would. Renew the login there, or shorten the test.",
  );
  expect({ approval: servers.approval.peek(), readiness: readiness("peer") }).toEqual({
    approval: { state: "renew", id: "peer", renewUrl: `${peer}/login?reason=renew` },
    readiness: "verified",
  });
  expect(await authorize(1_800_000)).toBe("covered");
  expect({ approval: servers.approval.peek(), readiness: readiness("peer") }).toEqual({
    approval: { state: "idle" },
    readiness: "verified",
  });
});
