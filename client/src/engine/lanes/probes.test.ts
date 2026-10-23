import { expect, test } from "bun:test";
import { DEADLINE_CEILING_MS } from "../measure/latency";
import type { ProbeChannelEvents, ServerAuth } from "../types";
import { elapse, FakeWorker, outcome, useFakeWorkers } from "./fakes.testutil";
import { openProbeChannel } from "./probes";

useFakeWorkers();

/** Opens a WebSocket bus whose sign-in check answers `ended` and whose server answers `fetch`, and keeps its events in order. */
function channel(
  ended: boolean,
  fetch: (url: string) => Promise<Response> = async () => new Response(null),
) {
  const events: [keyof ProbeChannelEvents, ...unknown[]][] = [];
  const auth: ServerAuth = {
    kind: "grant",
    left: () => Infinity,
    request: () => ({ headers: {}, credentials: "omit", redirect: "error" }),
    fetch,
    ticket: (origin, route) => ({
      url: `${origin}/ws/session?target=${route}`,
      request: auth.request("", "POST"),
    }),
    ended: async () => ended,
  };
  const bus = openProbeChannel(
    {
      target: {
        id: "https://peer.example",
        origin: "https://peer.example",
        transport: "websocket",
        protocol: "http1",
        tls: true,
      },
      auth,
      intervalMs: 80,
      maxInFlight: 2,
    },
    new Proxy({} as Required<ProbeChannelEvents>, {
      get:
        (_, type: keyof ProbeChannelEvents) =>
        (...args: unknown[]) =>
          events.push([type, ...args]),
    }),
  );
  return { bus, events, worker: FakeWorker.spawned.at(-1)! };
}

test("outcomes cross onto page time, and a stalled bus that the sign-in check finds ended asks for sign-in", async () => {
  const { events, worker } = channel(true);
  expect(worker.commands).toEqual([
    {
      type: "start",
      url: "wss://peer.example/ws/ping",
      transport: "websocket",
      ticket: {
        url: "https://peer.example/ws/session?target=/ws/ping",
        request: { headers: {}, credentials: "omit", redirect: "error" },
      },
      intervalMs: 80,
      maxInFlight: 2,
    },
  ]);
  worker.report({ type: "ready" });
  const outcome = { rttMs: 12, timedOut: false, handlingMs: null, inWindow: true };
  worker.report({
    type: "outcomes",
    outcomes: [{ ...outcome, observedAt: performance.timeOrigin + 1_234 }],
  });
  worker.report({ type: "stalled", detail: "websocket closed" });
  await elapse(0);
  expect(events).toEqual([
    ["ready"],
    ["outcomes", [{ ...outcome, observedAt: 1_234 }]],
    ["stalled", "websocket closed"],
    ["signInRequired"],
  ]);
  expect(worker.terminated).toBe(true);
});

test("a window's drain waits at most the longest deadline and a margin; acknowledged, the bus stays, cut off, it ends incomplete", async () => {
  for (const acknowledged of [true, false]) {
    const { bus, events, worker } = channel(false);
    bus.measure();
    const drained = outcome(bus.stop());
    expect(worker.commands.slice(1)).toEqual([{ type: "measure" }, { type: "stop" }]);
    if (acknowledged) worker.report({ type: "drained" });
    await elapse(DEADLINE_CEILING_MS + 245);
    expect(drained.at).toBe(acknowledged ? 0 : undefined);
    await elapse(5);
    expect(drained.at).toBe(acknowledged ? 0 : DEADLINE_CEILING_MS + 250);
    expect(worker.terminated).toBe(!acknowledged);
    expect(events).toEqual(acknowledged ? [] : [["incomplete"]]);
    // Only a window that was measured can be left incomplete.
    bus.dispose(true);
    expect(events).toEqual(acknowledged ? [] : [["incomplete"]]);
    expect(worker.terminated).toBe(true);
  }
});

test("an adopted bus reports to its new owner, told at once whether it answers or stalled; an ended one is not adopted", () => {
  const { bus, events, worker } = channel(false);
  const next: [keyof ProbeChannelEvents, ...unknown[]][] = [];
  const owner = new Proxy({} as Required<ProbeChannelEvents>, {
    get:
      (_, type: keyof ProbeChannelEvents) =>
      (...args: unknown[]) =>
        next.push([type, ...args]),
  });
  expect(bus.adopt(owner)).toBe(true);
  worker.report({ type: "ready" });
  expect(bus.adopt(owner)).toBe(true);
  worker.report({ type: "stalled", detail: "websocket closed" });
  expect(bus.adopt(owner)).toBe(true);
  worker.report({ type: "resumed" });
  expect(bus.adopt(owner)).toBe(true);
  expect(next).toEqual([
    ["ready"],
    ["ready"],
    ["stalled", "websocket closed"],
    ["stalled", "websocket closed"],
    ["resumed"],
    ["ready"],
  ]);
  expect(events).toEqual([]);
  bus.dispose();
  expect(bus.adopt(owner)).toBe(false);
  expect(worker.terminated).toBe(true);
});

test("a stalled bus asks over HTTP every half second until its server answers, then redials once per owner", async () => {
  let answering = false;
  const asked: string[] = [];
  const { bus, worker } = channel(false, async (url) => {
    asked.push(url);
    if (!answering) throw new TypeError("network error");
    return new Response(null);
  });
  const dials = () => worker.commands.filter(({ type }) => type === "dial").length;
  worker.report({ type: "stalled", detail: "websocket closed" });
  await elapse(1_000);
  expect([asked.length, dials()]).toEqual([3, 0]);
  answering = true;
  await elapse(500);
  expect([asked, dials()]).toEqual([Array(4).fill("https://peer.example/probe"), 1]);
  worker.report({ type: "resumed" });
  worker.report({ type: "stalled", detail: "websocket closed" });
  await elapse(1_000);
  expect([asked.length, dials()]).toEqual([4, 1]);
  // The next stage's owner gets its own redial.
  bus.adopt(new Proxy({} as Required<ProbeChannelEvents>, { get: () => () => {} }));
  await elapse(0);
  expect([asked.length, dials()]).toEqual([5, 2]);
});
