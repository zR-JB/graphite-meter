import { expect, test } from "bun:test";
import { DEADLINE_CEILING_MS } from "../measure/latency";
import type { ProbeChannelEvents, ServerAuth } from "../types";
import { elapse, FakeWorker, useFakeWorkers } from "./fakes.testutil";
import { openProbeChannel } from "./probes";

useFakeWorkers();

/** A WebSocket bus whose sign-in check answers `ended`; its events are kept in order. */
function channel(ended: boolean) {
  const events: [keyof ProbeChannelEvents, ...unknown[]][] = [];
  const auth: ServerAuth = {
    kind: "grant",
    deadline: null,
    request: () => ({ headers: {}, credentials: "omit", redirect: "error" }),
    fetch: async () => new Response(null),
    ticket: (origin, route) => ({ url: `${origin}/ws/session?target=${route}`, request: auth.request("", "POST") }),
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
  worker.report({ type: "outcomes", outcomes: [{ ...outcome, observedAt: performance.timeOrigin + 1_234 }] });
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

test("a stop waits for the drain at most the longest deadline and a margin; one cut off leaves the population incomplete", async () => {
  for (const acknowledged of [true, false]) {
    const { bus, events, worker } = channel(false);
    bus.measure();
    void bus.stop();
    expect(worker.commands.slice(1)).toEqual([{ type: "measure" }, { type: "stop" }]);
    if (acknowledged) worker.report({ type: "stopped" });
    await elapse(DEADLINE_CEILING_MS + 245);
    expect(worker.terminated).toBe(acknowledged);
    await elapse(5);
    expect(worker.terminated).toBe(true);
    expect(events).toEqual(acknowledged ? [] : [["incomplete"]]);
  }
});
