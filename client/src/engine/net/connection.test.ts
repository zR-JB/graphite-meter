import { afterEach, expect, test } from "bun:test";
import { effect, signal } from "../../lib/signal";
import type { CatalogEntry } from "../../protocol/documents";
import type { ProbeOutcome, ServerAuth } from "../types";
import { grantAuth } from "./auth";
import { createConnection, type ConnectionEnv } from "./connection";
import { FakeBuses, fakeNet, h, page, preflight, PROBE, publicAuth, ws } from "./fakes.testutil";
import { createLimiter } from "./limiter";
import { createPageSession } from "./page";
import { parseSelection } from "./paths";

const self: CatalogEntry = { id: "self", url: page.origin, name: "Meter", additionalOrigins: [] };
let net: ReturnType<typeof fakeNet>;
const cleanups: (() => void)[] = [];
afterEach(() => {
  for (const cleanup of cleanups.splice(0)) cleanup();
  net.restore();
});

/** A selected server's connection on the fake network, and every readiness its view painted. */
function connect(
  entry: CatalogEntry,
  { auth = publicAuth(entry) as ServerAuth, latency = false, shown = true, now = net.now } = {},
) {
  const buses = new FakeBuses();
  const idle: ProbeOutcome[] = [];
  const env = {
    page,
    session: createPageSession(page, net.now),
    limiter: createLimiter(),
    openProbeChannel: buses.open,
    active: signal(true),
    hidden: signal(false),
    online: signal(true),
    metadata: signal(false),
    latencyServer: signal(shown ? entry.id : null),
    selections: {
      throughput: signal(parseSelection("auto")),
      latency: signal(parseSelection("auto")),
    },
    latencyNeeded: signal(latency),
    idleLatency: (_: string, outcomes: readonly ProbeOutcome[]) => idle.push(...outcomes),
    now,
  } satisfies ConnectionEnv;
  const selected = signal(true);
  const connection = createConnection(entry, auth, selected, env);
  const readiness: string[] = [];
  let painting = false;
  const stop = effect(() => {
    connection.view();
    if (painting) return;
    painting = true;
    setImmediate(() => {
      painting = false;
      const state = connection.view.peek().readiness;
      if (readiness.at(-1) !== state) readiness.push(state);
    });
  });
  cleanups.push(stop, connection.dispose);
  return { connection, env, selected, buses, idle, readiness };
}

test("a failed discovery is read again each minute, resume makes it due, and a grant's end waits for the user", async () => {
  let up = false;
  net = fakeNet((url) =>
    !up
      ? "unreachable"
      : url.pathname === "/preflight"
        ? { body: preflight([h(".", "http1")]) }
        : { body: PROBE, hop: "http/1.1" },
  );
  const peer: CatalogEntry = {
    id: "peer",
    url: "https://peer.example",
    name: "Peer",
    additionalOrigins: [],
  };
  const grant = {
    token: "t".repeat(43),
    deadline: 120_000,
    expires: Infinity,
    maximumLifetimeMs: 28_800_000,
  };
  const { connection } = connect(peer, { auth: grantAuth(peer, grant, net.now) });
  await net.advance(20_000);
  expect(net.sent("/preflight")).toEqual([0]);
  expect(connection.view.peek().message).toBe("Could not reach this test server");
  connection.resume();
  await net.advance(61_000);
  expect(net.sent("/preflight")).toEqual([0, 20_000, 80_000]);
  up = true;
  connection.resume();
  await net.advance(0);
  expect(connection.view.peek().readiness).toBe("verified");
  await net.advance(45_000);
  expect(connection.view.peek()).toMatchObject({
    readiness: "sign-in",
    blocked: "Sign in to Meter",
  });
  // Sign-in is never retried on its own, and an ended grant is refused before any request.
  const sent = net.requests.length;
  connection.resume();
  await net.advance(60_000);
  await connection.check({ force: true });
  expect(net.requests.length).toBe(sent);
});

test("a device that says it is offline still checks its servers, and says offline only when a check fails", async () => {
  let reachable = true;
  net = fakeNet((url) =>
    !reachable
      ? "unreachable"
      : url.pathname === "/preflight"
        ? { body: preflight([h(".", "http1")]) }
        : { body: PROBE, hop: "http/1.1" },
  );
  // A lab link without a gateway: the browser reports offline, the server answers.
  const peer: CatalogEntry = {
    id: "peer",
    url: "https://peer.example",
    name: "Peer",
    additionalOrigins: [],
  };
  const { connection, env } = connect(peer);
  env.online.set(false);
  await net.advance(1_000);
  expect(connection.view.peek()).toMatchObject({
    readiness: "verified",
    message: null,
    blocked: null,
  });
  expect(connection.prepared(0)).not.toBeNull();
  reachable = false;
  await net.advance(60_000);
  expect(connection.view.peek()).toMatchObject({
    readiness: "failed",
    message: "This device is offline",
    blocked: null,
  });
  // Start tries again rather than being held back.
  reachable = true;
  await connection.check({ since: net.now() });
  expect(connection.prepared(net.now() - 1_000)).not.toBeNull();
});

test("a selected server re-reads discovery each minute without a checking flash or moving Automatic", async () => {
  const hops = ["h2", null, "h3"];
  let alive = true;
  net = fakeNet((url) => {
    if (!alive) return "unreachable";
    if (url.pathname !== "/preflight") return { body: PROBE, afterMs: 100 };
    const offer = preflight([h(".", "negotiated"), h("https://meter.example:8443", "http3")]);
    return {
      body: offer,
      afterMs: 100,
      hop: hops[Math.min(net.sent("/preflight").length - 1, hops.length - 1)],
    };
  });
  const { connection, readiness } = connect(self);
  await net.advance(140_000);
  expect(net.sent("/preflight")).toEqual([0, 60_100, 120_200]);
  // The negotiated head, seen over HTTP/2 first, stays verified through a re-read without a hop and one over HTTP/3.
  expect(net.sent("/probe")).toEqual([100]);
  expect(connection.view.peek().throughput.path?.fetch).toMatchObject({
    origin: page.origin,
    protocol: "http2",
  });
  alive = false;
  await net.advance(60_000);
  expect(connection.view.peek().message).toBe("Could not reach this test server");
  expect(readiness).toEqual(["checking", "verified", "failed"]);
});

test("a timer that fires before a coarse page clock reaches its time still re-reads, so a dead peer fails", async () => {
  let alive = true;
  net = fakeNet((url) =>
    !alive
      ? "unreachable"
      : url.pathname === "/preflight"
        ? { body: preflight([h(".", "http1")]) }
        : { body: PROBE, hop: "http/1.1" },
  );
  // Firefox's page clock may read up to a millisecond ahead of the timer that wakes it.
  let aheadMs = 0.5;
  const { connection } = connect(self, { now: () => net.now() + aheadMs });
  await net.advance(1_000);
  aheadMs = 0;
  alive = false;
  await net.advance(60_000);
  expect(net.sent("/preflight")).toEqual([0, 60_001]);
  expect(connection.view.peek().message).toBe("Could not reach this test server");
});

test("a server selected again keeps evidence up to 2 min old and proves older evidence again", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const { connection, selected } = connect(self);
  await net.advance(1_000);
  for (const [awayMs, preflights, probes] of [
    [60_000, [0, 61_000], [0]],
    [121_000, [0, 61_000, 182_000], [0, 182_000]],
  ] as const) {
    selected.set(false);
    await net.advance(awayMs);
    selected.set(true);
    await net.advance(0);
    expect<unknown>({ preflights: net.sent("/preflight"), probes: net.sent("/probe") }).toEqual({
      preflights,
      probes,
    });
    expect(connection.view.peek().readiness).toBe("verified");
  }
});

test("the bus that proved the shown server's latency monitors it, gives way to a new check's after a stall, and stays once not shown", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")], [ws(".")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const { connection, env, buses, idle } = connect(self, { latency: true });
  await net.advance(30_000);
  // One bus served the check at its 120 ms cadence, then the idle console at 1 s, in place of liveness reads.
  const monitor = buses.opened[0]!;
  expect(buses.opened).toEqual([monitor]);
  expect({
    opened: monitor.options.intervalMs,
    paced: monitor.intervalMs,
    disposed: monitor.disposed,
  }).toEqual({
    opened: 120,
    paced: 1_000,
    disposed: false,
  });
  expect(connection.view.peek()).toMatchObject({
    readiness: "verified",
    idle: true,
    connectivity: "connected",
  });
  expect(idle.length).toBe(30);
  expect(net.sent("/preflight")).toEqual([0]);
  monitor.events.stalled("connection lost");
  expect(connection.view.peek().connectivity).toBe("offline");
  await net.advance(1_000);
  expect(buses.opened.map(({ disposed, intervalMs }) => [disposed, intervalMs])).toEqual([
    [true, 1_000],
    [false, 1_000],
  ]);
  expect(net.sent("/probe")).toEqual([0, 0, 30_000]);
  expect(connection.view.peek()).toMatchObject({
    readiness: "verified",
    idle: true,
    connectivity: "connected",
  });
  env.latencyServer.set(null);
  await net.advance(30_000);
  expect(buses.opened.map(({ disposed }) => disposed)).toEqual([true, false]);
  expect(net.sent("/preflight")).toEqual([0, 60_000]);
});

test("a monitored server re-reads its discovery each minute, quietly, and a new generation proves its paths again", async () => {
  let generation = "g1";
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: { ...preflight([h(".", "http1")], [ws(".")]), generation } }
      : { body: PROBE, hop: "http/1.1" },
  );
  const { connection, buses, readiness } = connect(self, { latency: true });
  await net.advance(119_000);
  expect(net.sent("/preflight")).toEqual([0, 60_000]);
  expect(net.sent("/probe")).toEqual([0, 0]);
  expect(buses.opened.map(({ disposed }) => disposed)).toEqual([false]);
  expect(readiness).toEqual(["checking", "verified"]);
  // A restart behind a proxy that keeps the bus open.
  generation = "g2";
  await net.advance(3_000);
  expect(net.sent("/preflight")).toEqual([0, 60_000, 120_000]);
  expect(net.sent("/probe")).toEqual([0, 0, 120_000, 120_000]);
  expect(buses.opened.map(({ disposed }) => disposed)).toEqual([false]);
  expect(connection.view.peek()).toMatchObject({
    readiness: "verified",
    idle: true,
    discovery: { generation: "g2" },
  });
});

test("a latency check's bus stays with its server, unshown too, and closes when the check is cut short or the server goes", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")], [ws(".")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const unshown = connect(self, { latency: true, shown: false });
  await net.advance(1_000);
  expect(unshown.connection.view.peek().readiness).toBe("verified");
  expect(unshown.buses.opened.map(({ disposed }) => disposed)).toEqual([false]);
  const cut = connect(self, { latency: true });
  await net.advance(200);
  cut.env.selections.latency.set(parseSelection("transport:websocket"));
  expect(cut.buses.opened.map(({ disposed }) => disposed)).toEqual([true]);
  await net.advance(1_000);
  expect(cut.buses.opened.map(({ disposed }) => disposed)).toEqual([true, false]);
  cut.connection.dispose();
  expect(cut.buses.opened.map(({ disposed }) => disposed)).toEqual([true, true]);
});

test("a failed path check waits for a reason to check again, never for a timer", async () => {
  let probing = false;
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")]) }
      : probing
        ? { body: PROBE, hop: "http/1.1" }
        : "unreachable",
  );
  const { connection } = connect(self);
  await net.advance(300_000);
  expect(net.sent("/probe")).toEqual([0]);
  expect(connection.view.peek().readiness).toBe("failed");
  probing = true;
  await connection.check({ since: net.now() });
  expect(net.sent("/probe")).toEqual([0, 300_000]);
  expect(connection.view.peek().readiness).toBe("verified");
});

test("a run that measured over a path without failing it proves it, as a check would", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const { connection } = connect(self);
  await net.advance(130_000);
  const since = net.now() - 120_000;
  expect(connection.prepared(since)).toBeNull();
  connection.proved(["throughput"]);
  expect(connection.prepared(since)?.throughput.verifiedAt).toBe(net.now());
  expect(net.sent("/probe")).toEqual([0]);
});

test("a path a run failed on is proved again at the next start, not at once", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const { connection } = connect(self);
  await net.advance(1_000);
  connection.invalidate(["throughput"], true);
  await net.advance(90_000);
  expect(net.sent("/preflight")).toEqual([0, 60_000]);
  expect(net.sent("/probe")).toEqual([0]);
  expect(connection.view.peek().readiness).toBe("unchecked");
  await connection.check({ since: net.now() });
  expect(net.sent("/probe")).toEqual([0, 91_000]);
});

test("a parked bus that went quiet while a run held the checks is proved again once checks may run", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")], [ws(".")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const { env, buses } = connect(self, { latency: true });
  await net.advance(1_000);
  env.active.set(false);
  buses.opened[0]!.events.stalled("connection lost");
  await net.advance(5_000);
  expect(buses.opened.map(({ disposed }) => disposed)).toEqual([true]);
  expect(net.sent("/probe")).toEqual([0, 0]);
  env.active.set(true);
  await net.advance(1_000);
  expect(net.sent("/probe")).toEqual([0, 0, 6_000]);
  expect(buses.opened.map(({ disposed }) => disposed)).toEqual([true, false]);
});

test("transfer workers no run took between two timed discovery reads end at the second", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const terminated: string[] = [];
  const Real = globalThis.Worker;
  globalThis.Worker = class {
    constructor(readonly url: string) {}
    terminate() {
      terminated.push(this.url);
    }
  } as unknown as typeof Worker;
  try {
    const { connection } = connect(self);
    await net.advance(1_000);
    const { workers } = connection.link;
    workers.give("lane", workers.take("lane"));
    await net.advance(60_000);
    expect(terminated).toEqual([]);
    await net.advance(60_000);
    expect(terminated).toHaveLength(1);
  } finally {
    globalThis.Worker = Real;
  }
});

test("a server of another protocol is named either way, waits for the user, and is read again only on Retry", async () => {
  // A 0.13.4 server's preflight: no protocol and no lease.
  const older = { ...preflight([h(".", "http1")]), engineVersion: "0.13.4", lease: undefined };
  let body: object = older;
  net = fakeNet((url) =>
    url.pathname === "/preflight" ? { body } : { body: PROBE, hop: "http/1.1" },
  );
  const { connection } = connect(self);
  await net.advance(0);
  expect(connection.view.peek().message).toBe(
    "Meter runs Graphite Meter 0.13.4 (protocol 1); this page needs protocol 2, from 0.13.5. Update Meter.",
  );
  connection.resume();
  await net.advance(300_000);
  await connection.check({ since: net.now() });
  expect(net.sent("/preflight")).toEqual([0]);
  body = { ...preflight([h(".", "http1")]), engineVersion: "0.15.0", protocol: 3 };
  await connection.check({ force: true });
  expect(connection.view.peek().message).toBe(
    "Meter runs Graphite Meter 0.15.0, newer than this page (9.9.9); reload the page or update the server this page comes from.",
  );
  body = { ...preflight([h(".", "http1")]), protocol: 2 };
  await connection.check({ force: true });
  await net.advance(0);
  expect(connection.view.peek().readiness).toBe("verified");
});
