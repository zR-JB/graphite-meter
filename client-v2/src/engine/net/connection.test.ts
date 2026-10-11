import { afterEach, expect, test } from "bun:test";
import { effect, signal } from "../../lib/signal";
import type { CatalogEntry } from "../../protocol/documents";
import type { ProbeOutcome, ServerAuth } from "../types";
import { grantAuth, publicAuth } from "./auth";
import { createConnection, type ConnectionEnv } from "./connection";
import { FakeBuses, fakeNet, h, page, preflight, PROBE, ws } from "./fakes";
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

/** A selected server's connection on the fake network, and every readiness its view painted: what it shows once the
    task that changed it is done. */
function connect(entry: CatalogEntry, { auth = publicAuth(entry) as ServerAuth, latency = false, shown = true } = {}) {
  const buses = new FakeBuses();
  const idle: ProbeOutcome[] = [];
  const env = {
    page,
    session: createPageSession(page, net.now),
    limiter: createLimiter(),
    openProbeChannel: buses.open,
    active: signal(true),
    online: signal(true),
    metadata: signal(false),
    latencyServer: signal(shown ? entry.id : null),
    selections: { throughput: signal(parseSelection("auto")), latency: signal(parseSelection("auto")) },
    latencyNeeded: signal(latency),
    checkpoints: signal(false),
    idleLatency: (_: string, outcomes: readonly ProbeOutcome[]) => idle.push(...outcomes),
    now: net.now,
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

test("failed checks back off 5 s then 10 s, resume makes them due, and a grant's end waits for the user", async () => {
  let up = false;
  net = fakeNet((url) =>
    !up
      ? "unreachable"
      : url.pathname === "/preflight"
        ? { body: preflight([h(".", "http1")]) }
        : { body: PROBE, hop: "http/1.1" },
  );
  const peer: CatalogEntry = { id: "peer", url: "https://peer.example", name: "Peer", additionalOrigins: [] };
  const grant = { token: "t".repeat(43), deadline: 60_000, maximumLifetimeMs: 28_800_000 };
  const { connection } = connect(peer, { auth: grantAuth(peer, grant, net.now) });
  await net.advance(20_000);
  expect(net.sent("/preflight")).toEqual([0, 5_000, 15_000]);
  expect(connection.view.peek().message).toBe("Server could not be reached");
  connection.resume();
  await net.advance(0);
  expect(net.sent("/preflight")).toEqual([0, 5_000, 15_000, 20_000]);
  up = true;
  connection.resume();
  await net.advance(0);
  expect(connection.view.peek().readiness).toBe("verified");
  await net.advance(45_000);
  expect(connection.view.peek()).toMatchObject({ readiness: "sign-in", blocked: "Sign in to Meter" });
  // Sign-in is never retried on its own, and an ended grant is refused before any request.
  const sent = net.requests.length;
  connection.resume();
  await net.advance(60_000);
  await connection.check({ force: true });
  expect(net.requests.length).toBe(sent);
});

test("a selected server re-reads discovery every 5 s without a checking flash or moving Automatic", async () => {
  const hops = ["h2", null, "h3"];
  let alive = true;
  net = fakeNet((url) => {
    if (!alive) return "unreachable";
    if (url.pathname !== "/preflight") return { body: PROBE, afterMs: 100 };
    const offer = preflight([h(".", "negotiated"), h("https://meter.example:8443", "http3")]);
    return { body: offer, afterMs: 100, hop: hops[Math.min(net.sent("/preflight").length - 1, hops.length - 1)] };
  });
  const { connection, readiness } = connect(self);
  await net.advance(14_000);
  expect(net.sent("/preflight")).toEqual([0, 5_100, 10_200]);
  // The negotiated head, seen over HTTP/2 first, stays verified through a re-read without a hop and one over HTTP/3.
  expect(net.sent("/probe")).toEqual([100]);
  expect(connection.view.peek().throughput.path?.fetch).toMatchObject({ origin: page.origin, protocol: "http2" });
  alive = false;
  await net.advance(5_000);
  expect(connection.view.peek().message).toBe("Server could not be reached");
  expect(readiness).toEqual(["checking", "verified", "failed"]);
});

test("a server selected again keeps evidence up to 2 min old and proves older evidence again", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight" ? { body: preflight([h(".", "http1")]) } : { body: PROBE, hop: "http/1.1" },
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
    expect<unknown>({ preflights: net.sent("/preflight"), probes: net.sent("/probe") }).toEqual({ preflights, probes });
    expect(connection.view.peek().readiness).toBe("verified");
  }
});

test("the bus that proved the shown server's latency becomes its idle monitor, and every other bus closes", async () => {
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
  expect({ opened: monitor.options.intervalMs, paced: monitor.intervalMs, disposed: monitor.disposed }).toEqual({
    opened: 120,
    paced: 1_000,
    disposed: false,
  });
  expect(connection.view.peek()).toMatchObject({ readiness: "verified", idle: true, connectivity: "connected" });
  expect(idle.length).toBe(30);
  expect(net.sent("/preflight")).toEqual([0]);
  // A bus that went quiet leaves its path to be proven again, and the new check's bus monitors next.
  monitor.events.stalled("connection lost");
  expect(connection.view.peek().connectivity).toBe("offline");
  await net.advance(1_000);
  expect(buses.opened.map(({ disposed, intervalMs }) => [disposed, intervalMs])).toEqual([
    [true, 1_000],
    [false, 1_000],
  ]);
  expect(net.sent("/probe")).toEqual([0, 0, 30_000]);
  expect(connection.view.peek()).toMatchObject({ readiness: "verified", idle: true, connectivity: "connected" });
  // A server no longer shown is no longer monitored, and its discovery is re-read again.
  env.latencyServer.set(null);
  expect(buses.opened[1]!.disposed).toBe(true);
  await net.advance(5_000);
  expect(net.sent("/preflight")).toEqual([0, 30_000, 35_000]);
});

test("a latency check's bus closes with the check when it is cut short or no monitor wants it", async () => {
  net = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight([h(".", "http1")], [ws(".")]) }
      : { body: PROBE, hop: "http/1.1" },
  );
  const unshown = connect(self, { latency: true, shown: false });
  await net.advance(1_000);
  expect(unshown.connection.view.peek().readiness).toBe("verified");
  expect(unshown.buses.opened.map(({ disposed }) => disposed)).toEqual([true]);
  const cut = connect(self, { latency: true });
  await net.advance(200);
  cut.env.selections.latency.set(parseSelection("transport:websocket"));
  expect(cut.buses.opened.map(({ disposed }) => disposed)).toEqual([true]);
  await net.advance(1_000);
  expect(cut.buses.opened.map(({ disposed }) => disposed)).toEqual([true, false]);
  cut.connection.dispose();
  expect(cut.buses.opened.map(({ disposed }) => disposed)).toEqual([true, true]);
});
