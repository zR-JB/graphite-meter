import { expect, test } from "bun:test";
import type { Activity } from "../run/schedule";
import {
  ServerBusyError,
  type FetchTarget,
  type PreparedServer,
  type ServerAuth,
  type SessionTarget,
  type StageEvents,
  type StageSetup,
} from "../types";
import { elapse, FakeWorker, outcome, useFakeWorkers } from "./fakes.testutil";
import { openStage } from "./stage";

useFakeWorkers();

const ORIGIN = "https://peer.example";
const FETCH: FetchTarget = { id: ORIGIN, origin: ORIGIN, transport: "fetch-stream", protocol: "http2", tls: true };
const SESSION: SessionTarget = {
  id: `${ORIGIN}::wt`,
  origin: ORIGIN,
  transport: "webtransport",
  protocol: "http3",
  tls: true,
};
const DOWNLOAD: Activity = { stage: "download", transfer: ["down"], loadedLatency: false };

/** One server's stage over `target`, whose credentials answer with `auth` and whose events are kept in order; its
    latency bus is `bus`. */
function stage(
  activity: Activity,
  auth: Partial<ServerAuth> = {},
  target: FetchTarget | SessionTarget = FETCH,
  bus: StageSetup["bus"] = { current: null },
) {
  const events: [keyof StageEvents, ...unknown[]][] = [];
  const spawned = FakeWorker.spawned.length;
  let checks = 0;
  const credentials: ServerAuth = {
    kind: "grant",
    deadline: null,
    request: () => ({ headers: { authorization: "Bearer g" }, credentials: "omit", redirect: "error" }),
    fetch: async () => new Response(null, { status: 500 }),
    ticket: () => null,
    ended: async () => (checks++, false),
    ...auth,
  };
  const resources = openStage({
    server: {
      server: { id: "peer", url: ORIGIN, name: "Peer" },
      auth: credentials,
      throughput: { target, fetch: FETCH },
      latency: { target: { id: ORIGIN, origin: ORIGIN, transport: "websocket", protocol: "http1", tls: true } },
    } as unknown as PreparedServer,
    activity,
    streams: { down: activity.transfer.includes("down") ? 3 : 0, up: activity.transfer.includes("up") ? 1 : 0 },
    gating: { down: 2, up: 0 },
    cadence: "fast",
    seed: "s",
    events: new Proxy({} as StageEvents, {
      get:
        (_, type: keyof StageEvents) =>
        (...args: unknown[]) =>
          events.push([type, ...args]),
    }),
    now: () => performance.now(),
    bus,
  });
  const workers = (url: string) => FakeWorker.spawned.slice(spawned).filter((worker) => worker.url === url);
  return {
    resources,
    events,
    lanes: () => workers("lane"),
    session: () => workers("session")[0]!,
    probes: () => workers("probe"),
    checks: () => checks,
  };
}

const bytes = (lane: FakeWorker, epoch: number) => lane.report({ type: "bytes", bytes: 1_000, epoch });

test("readiness waits for the gating lanes; a lane's failure keeps its reason; one budget bounds a hung mint", async () => {
  const ready = stage(DOWNLOAD);
  const prepared = outcome(ready.resources.prepare(new AbortController().signal));
  await elapse(150);
  const [first, second, third] = ready.lanes();
  expect(ready.lanes().map(({ commands }) => commands[0]?.type)).toEqual(["download", "download", "download"]);
  bytes(first!, 0);
  await elapse(5);
  expect(prepared.at).toBeUndefined();
  bytes(third!, 0);
  await elapse(5);
  expect(prepared).toEqual({ at: 155 });
  expect(second!.terminated).toBe(false);

  // An unexpected answer fails the preparation with its own reason, which the run reports.
  const refused = stage(DOWNLOAD);
  const failed = outcome(refused.resources.prepare(new AbortController().signal));
  refused.lanes()[0]!.report({
    type: "ended",
    ending: { reason: "protocol-error", retry: false, detail: "HTTP 400" },
  });
  await elapse(5);
  expect(failed.error).toMatchObject({ reason: "protocol-error", message: "down stream 0 failed: HTTP 400" });
  expect(refused.lanes().every(({ terminated }) => terminated)).toBe(true);

  // The mint gives up at 3 s; its sign-in check answers 2.9 s later, long after the stage's 3.5 s.
  const hung = stage(
    { stage: "upload", transfer: ["up"], loadedLatency: false },
    {
      fetch: (_, init) =>
        new Promise((_, reject) => init!.signal!.addEventListener("abort", () => reject(init!.signal!.reason))),
      ended: () => new Promise((resolve) => setTimeout(() => resolve(false), 2_900)),
    },
  );
  const lapsed = outcome(hung.resources.prepare(new AbortController().signal));
  await elapse(7_000);
  expect(lapsed.at).toBe(3_500);
  expect(lapsed.error).toMatchObject({ message: "Primed measurement connections did not become ready" });
});

test("lanes refused as busy back off 300, 600, 1 200 ms, and the lapsed budget names the server busy", async () => {
  const busy = stage(DOWNLOAD);
  const prepared = outcome(busy.resources.prepare(new AbortController().signal));
  const [lane] = busy.lanes();
  const opened: number[] = [];
  for (let at = 0; at < 3_500; at += 5) {
    if (lane!.commands.length > opened.length) {
      opened.push(at);
      lane!.report({ type: "ended", ending: { reason: "server-busy", retry: true, detail: "HTTP 503" } });
    }
    await elapse(5);
  }
  expect(opened).toEqual([0, 300, 900, 2_100, 3_300]);
  await elapse(5);
  expect(prepared.error).toBeInstanceOf(ServerBusyError);
});

test("one download outage stalls once, checks the sign-in once, and resumes only when a reopened lane delivers", async () => {
  const outage = stage(DOWNLOAD);
  const prepared = outcome(outage.resources.prepare(new AbortController().signal));
  await elapse(150);
  const lanes = outage.lanes();
  for (const lane of lanes) bytes(lane, 0);
  await elapse(5);
  expect(prepared.error).toBeUndefined();
  outage.resources.measure();
  expect(lanes.map(({ commands }) => commands.at(-1))).toEqual(Array(3).fill({ type: "measure", epoch: 1 }));
  // A report counted before the window belongs to no measurement.
  bytes(lanes[0]!, 0);
  // Each lane reports the bytes it read before it saw the break, then the break.
  for (const lane of lanes) {
    bytes(lane, 1);
    lane.report({ type: "broken", detail: "TypeError: network error" });
  }
  // The outage lasts 2.5 s: every reopening breaks again.
  for (let round = 0; round < 8; round++) {
    await elapse(300);
    for (const lane of lanes) {
      expect(lane.commands.at(-1)).toMatchObject({ type: "download", epoch: 1 });
      lane.report({ type: "broken", detail: "TypeError: network error" });
    }
  }
  await elapse(300);
  bytes(lanes[1]!, 1);
  const transitions = outage.events.filter(([type]) => type === "stalled" || type === "resumed");
  expect(transitions).toEqual([
    ["stalled", { reason: "connection-lost", detail: "TypeError: network error", direction: "down" }],
    ["resumed"],
  ]);
  expect(outage.events.filter(([type]) => type === "download")).toHaveLength(4);
  expect(outage.checks()).toBe(1);
  outage.resources.discard();
  expect(lanes.every(({ terminated }) => terminated)).toBe(true);
});

test("a broken lane reopens after its pause while a slow sign-in check runs, which releases it if signed out", async () => {
  for (const signedOut of [false, true]) {
    const blip = stage(DOWNLOAD, {
      ended: () => new Promise((resolve) => setTimeout(() => resolve(signedOut), 2_000)),
    });
    void blip.resources.prepare(new AbortController().signal);
    await elapse(150);
    const lanes = blip.lanes();
    for (const lane of lanes) bytes(lane, 0);
    blip.resources.measure();
    lanes[0]!.report({ type: "broken", detail: "TypeError: network error" });
    await elapse(300);
    expect(lanes[0]!.commands.at(-1)).toMatchObject({ type: "download", epoch: 1 });
    await elapse(1_700);
    expect(blip.events.filter(([type]) => type === "signInRequired")).toEqual(
      signedOut ? [["signInRequired", "throughput"]] : [],
    );
    expect(lanes.every(({ terminated }) => terminated)).toBe(signedOut);
  }
});

test("a flush resolves once every lane answered what it counted by the cutoff, at most 250 ms", async () => {
  for (const answering of [3, 2]) {
    const flushing = stage(DOWNLOAD);
    void flushing.resources.prepare(new AbortController().signal);
    await elapse(150);
    const lanes = flushing.lanes();
    for (const lane of lanes) bytes(lane, 0);
    flushing.resources.measure();
    const cutoff = performance.now();
    const answer = flushing.resources.flush(cutoff);
    const flushed = outcome(answer);
    let read: number | null | undefined;
    void answer.then((value) => (read = value));
    const at = performance.timeOrigin + cutoff;
    expect(lanes.map(({ commands }) => commands.at(-1))).toEqual(Array(3).fill({ type: "flush", at }));
    // Each lane's report after the cutoff is in its answer's total; the one that never answers counts on.
    for (const lane of lanes) bytes(lane, 1);
    for (const lane of lanes.slice(0, answering)) lane.report({ type: "flushed", total: 1_000, at });
    await elapse(300);
    expect(flushed.at).toBe(answering === 3 ? 0 : 250);
    expect(read).toBe(cutoff);
    // A lane that answered counts nothing more: its later bytes lie past the boundary.
    for (const lane of lanes) bytes(lane, 1);
    expect(flushing.events.filter(([type]) => type === "download")).toHaveLength(answering + 2 * (3 - answering));
  }
});

test("lanes answering a flush at different times each count what they had by the one cutoff", async () => {
  const flushing = stage(DOWNLOAD);
  void flushing.resources.prepare(new AbortController().signal);
  await elapse(150);
  const lanes = flushing.lanes();
  flushing.resources.measure();
  for (const lane of lanes) bytes(lane, 1);
  const cutoff = performance.now();
  const answer = outcome(flushing.resources.flush(cutoff));
  const at = performance.timeOrigin + cutoff;
  // The first answers at once; the second's report sent before the cutoff arrives after it, and its answer 200 ms
  // later; the third had counted 400 more bytes by the cutoff, and had counted more by the time it answered.
  lanes[0]!.report({ type: "flushed", total: 1_000, at });
  bytes(lanes[1]!, 1);
  await elapse(200);
  lanes[1]!.report({ type: "flushed", total: 2_000, at });
  bytes(lanes[2]!, 1);
  lanes[2]!.report({ type: "flushed", total: 1_400, at });
  await elapse(0);
  expect(answer.at).toBe(200);
  const counted = flushing.events.flatMap(([type, bytes]) => (type === "download" ? [bytes as number] : []));
  expect(counted.reduce((sum, bytes) => sum + bytes, 0)).toBe(4_400);
});

test("a session's stop waits for its worker's acknowledgement, at most 2.5 s", async () => {
  for (const acknowledged of [true, false]) {
    const session = stage(DOWNLOAD, {}, SESSION);
    void session.resources.prepare(new AbortController().signal);
    const worker = session.session();
    expect(worker.commands).toEqual([
      {
        type: "download",
        url: `${ORIGIN}/wt/download?bytes=${64 * 1024 ** 3}&streams=3`,
        ticket: null,
        datagrams: false,
        epoch: 0,
      },
    ]);
    bytes(worker, 0);
    await elapse(0);
    const finished = outcome(session.resources.finish());
    expect(worker.commands.at(-1)).toEqual({ type: "stop" });
    if (acknowledged) worker.report({ type: "stopped" });
    await elapse(3_000);
    expect(finished.at).toBe(acknowledged ? 0 : 2_500);
    expect(worker.terminated).toBe(true);
  }
});

test("one latency bus serves the server's run: a later stage adopts and re-paces it, and only a discard closes it", async () => {
  const bus: StageSetup["bus"] = { current: null };
  const idle = stage({ stage: "latency", transfer: [], loadedLatency: false }, {}, FETCH, bus);
  const prepared = outcome(idle.resources.prepare(new AbortController().signal));
  const [probe] = idle.probes();
  expect(probe!.commands).toEqual([
    {
      type: "start",
      url: "wss://peer.example/ws/ping",
      transport: "websocket",
      ticket: null,
      intervalMs: 80,
      maxInFlight: 16,
    },
  ]);
  probe!.report({ type: "ready" });
  await elapse(0);
  expect(prepared).toEqual({ at: 0 });
  idle.resources.measure();
  const finished = outcome(idle.resources.finish());
  probe!.report({ type: "drained" });
  await elapse(0);
  expect(finished).toEqual({ at: 0 });
  expect(probe!.terminated).toBe(false);

  // The loaded stage dials nothing: its transfers never race a handshake.
  const loaded = stage({ stage: "download", transfer: ["down"], loadedLatency: true }, {}, FETCH, bus);
  const discarded = outcome(loaded.resources.prepare(new AbortController().signal));
  expect(loaded.probes()).toEqual([]);
  expect(probe!.commands.at(-1)).toEqual({ type: "pace", intervalMs: 80, maxInFlight: 2 });
  loaded.resources.measure();
  probe!.report({ type: "stalled", detail: "websocket closed" });
  expect(loaded.events.filter(([type]) => type === "latencyStalled")).toEqual([["latencyStalled", "websocket closed"]]);
  expect(idle.events.filter(([type]) => type === "latencyStalled")).toEqual([]);
  loaded.resources.discard(true);
  expect(probe!.terminated).toBe(true);
  expect(loaded.events.at(-1)).toEqual(["probesIncomplete"]);
  expect(bus.current).toBeNull();
  await elapse(0);
  expect(discarded.error).toBeInstanceOf(DOMException);
});
