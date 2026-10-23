import { expect, jest, test } from "bun:test";
import { handshakes, THROTTLED } from "../net/webtransport";
import type { Activity } from "../run/schedule";
import type {
  Discovery,
  FetchTarget,
  LatencyPath,
  LatencyTarget,
  PreparedServer,
  ServerAuth,
  SessionTarget,
  StageEvents,
  StageSetup,
} from "../types";
import { elapse, FakeWorker, outcome, useFakeWorkers } from "./fakes.testutil";
import { sessionClosed } from "./endings";
import { openStage } from "./stage";

useFakeWorkers();

const ORIGIN = "https://peer.example";
const FETCH: FetchTarget = {
  id: ORIGIN,
  origin: ORIGIN,
  transport: "fetch-stream",
  protocol: "http2",
  tls: true,
};
const SESSION: SessionTarget = {
  id: `${ORIGIN}::wt`,
  origin: ORIGIN,
  transport: "webtransport",
  protocol: "http3",
  tls: true,
};
const DOWNLOAD: Activity = { stage: "download", transfer: ["down"], loadedLatency: false };

/** One server's stage over `target`, with `auth` answering its credentials, `bus` as its latency bus and `server` overriding the rest. */
function stage(
  activity: Activity,
  auth: Partial<ServerAuth> = {},
  target: FetchTarget | SessionTarget = FETCH,
  bus: StageSetup["bus"] = { current: null },
  server: Partial<PreparedServer> = {},
) {
  const events: [keyof StageEvents, ...unknown[]][] = [];
  const spawned = FakeWorker.spawned.length;
  let checks = 0;
  const credentials: ServerAuth = {
    kind: "grant",
    left: () => Infinity,
    request: () => ({
      headers: { authorization: "Bearer g" },
      credentials: "omit",
      redirect: "error",
    }),
    fetch: async () => new Response(null),
    ticket: () => null,
    ended: async () => (checks++, false),
    ...auth,
  };
  const resources = openStage({
    server: {
      server: { id: "peer", url: ORIGIN, name: "Peer" },
      auth: credentials,
      throughput: { target, fetch: target.transport === "fetch-stream" ? target : FETCH },
      latency: {
        target: {
          id: ORIGIN,
          origin: ORIGIN,
          transport: "websocket",
          protocol: "http1",
          tls: true,
        },
      },
      ...server,
    } as unknown as PreparedServer,
    activity,
    streams: {
      down: activity.transfer.includes("down") ? 3 : 0,
      up: activity.transfer.includes("up") ? 1 : 0,
    },
    uploadLanes: activity.transfer.includes("up") ? 1 : 0,
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
  const workers = (url: string) =>
    FakeWorker.spawned.slice(spawned).filter((worker) => worker.url === url);
  return {
    resources,
    events,
    lanes: () => workers("lane"),
    session: () => workers("session")[0]!,
    probes: () => workers("probe"),
    checks: () => checks,
  };
}

const bytes = (lane: FakeWorker, epoch: number) =>
  lane.report({ type: "bytes", bytes: 1_000, epoch });

test("readiness waits for the gating lanes; a lane's failure keeps its reason; one budget bounds a hung mint", async () => {
  const ready = stage(DOWNLOAD);
  const prepared = outcome(ready.resources.prepare(new AbortController().signal));
  await elapse(150);
  const [first, second, third] = ready.lanes();
  expect(ready.lanes().map(({ commands }) => commands[0]?.type)).toEqual([
    "download",
    "download",
    "download",
  ]);
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
  expect(failed.error).toMatchObject({
    reason: "protocol-error",
    message: "down stream 0 failed: HTTP 400",
  });
  expect(refused.lanes().every(({ terminated }) => terminated)).toBe(true);

  // The mint gives up at 3 s. Its sign-in check answers 2.9 s later, long after the stage's 3.5 s.
  const hung = stage(
    { stage: "upload", transfer: ["up"], loadedLatency: false },
    {
      fetch: (_, init) =>
        new Promise((_, reject) =>
          init!.signal!.addEventListener("abort", () => reject(init!.signal!.reason)),
        ),
      ended: () => new Promise((resolve) => setTimeout(() => resolve(false), 2_900)),
    },
  );
  const lapsed = outcome(hung.resources.prepare(new AbortController().signal));
  await elapse(7_000);
  expect(lapsed.at).toBe(3_500);
  expect(lapsed.error).toMatchObject({
    message: "Primed measurement connections did not become ready",
  });
});

test("a lane refused as busy is asked again once, after 300 ms, and its second refusal fails the stage busy", async () => {
  const busy = stage(DOWNLOAD);
  const prepared = outcome(busy.resources.prepare(new AbortController().signal));
  const [lane] = busy.lanes();
  const opened: number[] = [];
  for (let at = 0; at < 3_500; at += 5) {
    if (lane!.commands.length > opened.length) {
      opened.push(at);
      lane!.report({
        type: "ended",
        ending: { reason: "server-busy", retry: true, detail: "HTTP 503" },
      });
    }
    await elapse(5);
  }
  expect(opened).toEqual([0, 300]);
  expect(prepared.error).toMatchObject({
    reason: "server-busy",
    message: "down stream 0 failed: HTTP 503",
  });
});

test("a broken lane reopens once, stalling once and checking the sign-in once; its second break fails the stage", async () => {
  const outage = stage(DOWNLOAD);
  const prepared = outcome(outage.resources.prepare(new AbortController().signal));
  await elapse(150);
  const lanes = outage.lanes();
  for (const lane of lanes) bytes(lane, 0);
  await elapse(5);
  expect(prepared.error).toBeUndefined();
  outage.resources.measure(1_000);
  expect(lanes.map(({ commands }) => commands.at(-1))).toEqual(
    Array(3).fill({ type: "measure", epoch: 1, at: performance.timeOrigin + 1_000 }),
  );
  // A report counted before the window belongs to no measurement.
  bytes(lanes[0]!, 0);
  // Each lane reports the bytes it read before it saw the break, then the break.
  for (const lane of lanes) {
    bytes(lane, 1);
    lane.report({ type: "broken", detail: "TypeError: network error" });
  }
  await elapse(300);
  for (const lane of lanes)
    expect(lane.commands.at(-1)).toMatchObject({ type: "download", epoch: 1 });
  bytes(lanes[1]!, 1);
  expect(outage.events.filter(([type]) => type === "stalled" || type === "resumed")).toEqual([
    [
      "stalled",
      { reason: "connection-lost", detail: "TypeError: network error", direction: "down" },
    ],
    ["resumed"],
  ]);
  expect(outage.checks()).toBe(1);
  lanes[0]!.report({ type: "broken", detail: "TypeError: network error" });
  expect(outage.events.filter(([type]) => type === "failed")).toEqual([
    ["failed", "connection-lost", "down stream 0 failed: TypeError: network error"],
  ]);
  expect(outage.events.filter(([type]) => type === "download")).toHaveLength(4);
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
    blip.resources.measure(performance.now());
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
    flushing.resources.measure(performance.now());
    const cutoff = performance.now();
    const answer = flushing.resources.flush(cutoff);
    const flushed = outcome(answer);
    let read: number | null | undefined;
    void answer.then((value) => (read = value));
    const at = performance.timeOrigin + cutoff;
    expect(lanes.map(({ commands }) => commands.at(-1))).toEqual(
      Array(3).fill({ type: "flush", at }),
    );
    // Each lane's report after the cutoff is in its answer's total. The one that never answers counts on.
    for (const lane of lanes) bytes(lane, 1);
    for (const lane of lanes.slice(0, answering))
      lane.report({ type: "flushed", total: 1_000, at });
    await elapse(300);
    expect(flushed.at).toBe(answering === 3 ? 0 : 250);
    // A lane that never answered counted on to the flush's end, where the counts then stand.
    expect(read).toBe(answering === 3 ? cutoff : cutoff + 250);
    // A lane that answered counts nothing more, since its later bytes lie past the boundary.
    for (const lane of lanes) bytes(lane, 1);
    expect(flushing.events.filter(([type]) => type === "download")).toHaveLength(
      answering + 2 * (3 - answering),
    );
  }
});

test("lanes answering a flush at different times each count what they had by the one cutoff", async () => {
  const flushing = stage(DOWNLOAD);
  void flushing.resources.prepare(new AbortController().signal);
  await elapse(150);
  const lanes = flushing.lanes();
  flushing.resources.measure(performance.now());
  for (const lane of lanes) bytes(lane, 1);
  const cutoff = performance.now();
  const answer = outcome(flushing.resources.flush(cutoff));
  const at = performance.timeOrigin + cutoff;
  // The first answers at once, the second's pre-cutoff report arrives late, and the third counted 400 more by the cutoff.
  lanes[0]!.report({ type: "flushed", total: 1_000, at });
  bytes(lanes[1]!, 1);
  await elapse(200);
  lanes[1]!.report({ type: "flushed", total: 2_000, at });
  bytes(lanes[2]!, 1);
  lanes[2]!.report({ type: "flushed", total: 1_400, at });
  await elapse(0);
  expect(answer.at).toBe(200);
  const counted = flushing.events.flatMap(([type, bytes]) =>
    type === "download" ? [bytes as number] : [],
  );
  expect(counted.reduce((sum, bytes) => sum + bytes, 0)).toBe(4_400);
});

test("a count sums every lane's total at a time without cutting the lanes, or is null without every answer", async () => {
  const counting = stage(DOWNLOAD);
  void counting.resources.prepare(new AbortController().signal);
  await elapse(150);
  const lanes = counting.lanes();
  counting.resources.measure(performance.now());
  for (const answering of [3, 2]) {
    let counted: number | null | undefined;
    const at = performance.now();
    void counting.resources.countAt(at).then((total) => (counted = total));
    expect(lanes.map(({ commands }) => commands.at(-1))).toEqual(
      Array(3).fill({ type: "flush", at: performance.timeOrigin + at }),
    );
    for (const [index, lane] of lanes.slice(0, answering).entries())
      lane.report({ type: "flushed", total: 1_000 * index, at });
    await elapse(300);
    expect(counted).toBe(answering === 3 ? 3_000 : null);
  }
  // Counting cuts no lane: every one counts on.
  for (const lane of lanes) bytes(lane, 1);
  expect(counting.events.filter(([type]) => type === "download")).toHaveLength(3);
  // The answer that came too late settles its own count, not the flush asked after it.
  const cutoff = performance.now();
  const flushed = outcome(counting.resources.flush(cutoff));
  lanes[2]!.report({ type: "flushed", total: 9_000, at: performance.timeOrigin + cutoff });
  for (const lane of lanes)
    lane.report({ type: "flushed", total: 1_000, at: performance.timeOrigin + cutoff });
  await elapse(0);
  expect(flushed.at).toBe(0);
  const counted = counting.events.flatMap(([type, bytes]) =>
    type === "download" ? [bytes as number] : [],
  );
  expect(counted.reduce((sum, bytes) => sum + bytes, 0)).toBe(3_000);
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

test("a broken session redials once its server answered over HTTP; one that does not answer within 3 s fails the stage", async () => {
  for (const answering of [true, false]) {
    const session = stage(
      DOWNLOAD,
      { fetch: async () => new Response(null, { status: answering ? 200 : 503 }) },
      SESSION,
    );
    void session.resources.prepare(new AbortController().signal);
    const worker = session.session();
    bytes(worker, 0);
    await elapse(0);
    worker.report({ type: "broken", detail: "webtransport session closed" });
    await elapse(3_500);
    expect(worker.commands.filter(({ type }) => type === "download")).toHaveLength(
      answering ? 2 : 1,
    );
    expect(session.events.filter(([type]) => type === "failed")).toEqual(
      answering
        ? []
        : [
            [
              "failed",
              "connection-lost",
              "down stream 0 failed: webtransport session closed; the server did not answer over HTTP",
            ],
          ],
    );
    session.resources.discard();
  }
});

test("a session's fourth failed handshake in the page throttles WebTransport: its redial and a new bus fail with why, loaded latency alone", async () => {
  try {
    const session = stage(DOWNLOAD, { fetch: async () => new Response(null) }, SESSION);
    void session.resources.prepare(new AbortController().signal);
    const worker = session.session();
    bytes(worker, 0);
    await elapse(0);
    for (let failed = 0; failed < 4; failed++) worker.report({ type: "handshake", opened: false });
    worker.report({ type: "broken", detail: "webtransport session closed" });
    await elapse(300);
    expect(worker.commands.filter(({ type }) => type === "download")).toHaveLength(1);
    expect(session.events.filter(([type]) => type === "failed")).toEqual([
      ["failed", "preparation-failed", THROTTLED],
    ]);
    const latency = { target: { ...SESSION, transport: "webtransport" } } as unknown as LatencyPath;
    const loaded = stage(
      { ...DOWNLOAD, loadedLatency: true },
      {},
      FETCH,
      { current: null },
      { latency },
    );
    outcome(loaded.resources.prepare(new AbortController().signal));
    expect([loaded.lanes().length, loaded.probes().length]).toEqual([3, 0]);
    expect(loaded.events.filter(([type]) => type === "latencyStalled")).toEqual([
      ["latencyStalled", THROTTLED],
    ]);
    const idle = stage(
      { stage: "latency", transfer: [], loadedLatency: false },
      {},
      FETCH,
      { current: null },
      { latency },
    );
    const prepared = outcome(idle.resources.prepare(new AbortController().signal));
    await elapse(0);
    expect(prepared.error).toMatchObject({ reason: "preparation-failed", message: THROTTLED });
    for (const each of [session, loaded, idle]) each.resources.discard();
  } finally {
    // The page's ledger lifts with time, for the tests after this one.
    await elapse(0);
    jest.advanceTimersByTime(5 * 60_000);
  }
});

test("a session discarded while it dials counts as a failed handshake, as Chrome counts it; one that delivered does not", async () => {
  try {
    const delivered = stage(DOWNLOAD, {}, SESSION);
    outcome(delivered.resources.prepare(new AbortController().signal));
    bytes(delivered.session(), 0);
    delivered.resources.discard();
    for (let abandoned = 0; abandoned < 3; abandoned++) {
      const dialing = stage(DOWNLOAD, {}, SESSION);
      outcome(dialing.resources.prepare(new AbortController().signal));
      dialing.resources.discard();
    }
    expect(handshakes.throttled()).toBe(false);
    const fourth = stage(DOWNLOAD, {}, SESSION);
    outcome(fourth.resources.prepare(new AbortController().signal));
    fourth.resources.discard();
    expect(handshakes.throttled()).toBe(true);
  } finally {
    await elapse(0);
    jest.advanceTimersByTime(5 * 60_000);
  }
});

test("a session ended at the server's bound reopens without spending the stage's one reopen", async () => {
  const session = stage(DOWNLOAD, { fetch: async () => new Response(null) }, SESSION);
  void session.resources.prepare(new AbortController().signal);
  const worker = session.session();
  bytes(worker, 0);
  await elapse(0);
  for (let cut = 0; cut < 3; cut++) {
    worker.report({ type: "ended", ending: sessionClosed(1)! });
    await elapse(300);
  }
  expect(worker.commands.filter(({ type }) => type === "download")).toHaveLength(4);
  expect(session.events.filter(([type]) => type === "failed")).toEqual([]);
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
  idle.resources.measure(performance.now());
  const finished = outcome(idle.resources.finish());
  probe!.report({ type: "drained" });
  await elapse(0);
  expect(finished).toEqual({ at: 0 });
  expect(probe!.terminated).toBe(false);

  // The loaded stage dials nothing, so its transfers never race a handshake.
  const loaded = stage(
    { stage: "download", transfer: ["down"], loadedLatency: true },
    {},
    FETCH,
    bus,
  );
  const discarded = outcome(loaded.resources.prepare(new AbortController().signal));
  expect(loaded.probes()).toEqual([]);
  expect(probe!.commands.at(-1)).toEqual({ type: "pace", intervalMs: 80, maxInFlight: 2 });
  loaded.resources.measure(performance.now());
  probe!.report({ type: "stalled", detail: "websocket closed" });
  expect(loaded.events.filter(([type]) => type === "latencyStalled")).toEqual([
    ["latencyStalled", "websocket closed"],
  ]);
  expect(idle.events.filter(([type]) => type === "latencyStalled")).toEqual([]);
  loaded.resources.discard(true);
  expect(probe!.terminated).toBe(true);
  expect(loaded.events.at(-1)).toEqual(["probesIncomplete"]);
  expect(bus.current).toBeNull();
  await elapse(0);
  expect(discarded.error).toBeInstanceOf(DOMException);
});

test("a feed closed by its upload's replacement while its server is asked fails nothing", async () => {
  const UPLOAD: Activity = { stage: "upload", transfer: ["up"], loadedLatency: false };
  const minted: string[] = [];
  const pending = (init?: RequestInit) =>
    new Promise<Response>((_, reject) =>
      init!.signal!.addEventListener("abort", () => reject(init!.signal!.reason)),
    );
  const replaced = stage(UPLOAD, {
    fetch: async (url, init) => {
      if (url.endsWith("/upload/session"))
        return Response.json({ uploadId: `u${minted.push(url)}` });
      if (url.includes("/upload/progress?id=u1")) throw new TypeError("network error");
      return pending(init);
    },
  });
  const prepared = outcome(replaced.resources.prepare(new AbortController().signal));
  await elapse(400);
  void replaced.resources.replaceUpload(new AbortController().signal);
  await elapse(100);
  expect(minted).toHaveLength(2);
  expect(prepared.error).toBeUndefined();
  replaced.resources.discard();
});

test("a fetch upload's feed reaches only its own origin: over a WebSocket offered there for proven HTTP/1.1, else over HTTP", async () => {
  const BUS = "https://bus.example";
  const socket = (origin: string): LatencyTarget => ({
    id: origin,
    origin,
    transport: "websocket",
    protocol: "http1",
    tls: true,
  });
  const offered = (...origins: string[]) =>
    new Map(origins.map((origin) => [origin, { blocked: null, targets: [socket(origin)] }]));
  const RealWebSocket = globalThis.WebSocket;
  try {
    for (const [why, protocol, sockets, feed] of [
      [
        "HTTP/1.1 beside its WebSocket",
        "http1",
        [BUS, ORIGIN],
        "wss://peer.example/ws/progress?id=u1",
      ],
      [
        "unproven HTTP, which may ride QUIC",
        "negotiated",
        [ORIGIN],
        `${ORIGIN}/upload/progress?id=u1`,
      ],
      [
        "HTTP/1.1 whose latency WebSocket is elsewhere",
        "http1",
        [BUS],
        `${ORIGIN}/upload/progress?id=u1`,
      ],
      ["HTTP/2", "http2", [ORIGIN], `${ORIGIN}/upload/progress?id=u1`],
      ["HTTP/3", "http3", [ORIGIN], `${ORIGIN}/upload/progress?id=u1`],
    ] as const) {
      const feeds: string[] = [];
      Object.assign(globalThis, {
        WebSocket: class {
          constructor(url: string) {
            feeds.push(url);
          }
          close() {}
        },
      });
      const upload = stage(
        { stage: "upload", transfer: ["up"], loadedLatency: false },
        {
          fetch: async (url, init) => {
            if (url === `${ORIGIN}/upload/session`) return Response.json({ uploadId: "u1" });
            if (url.includes("/upload/progress")) feeds.push(url);
            return new Promise((_, reject) =>
              init!.signal!.addEventListener("abort", () => reject(init!.signal!.reason)),
            );
          },
        },
        { ...FETCH, protocol },
        { current: null },
        {
          discovery: { latency: offered(...sockets) } as Discovery,
          latency: { target: socket(BUS) } as LatencyPath,
        },
      );
      void upload.resources.prepare(new AbortController().signal).catch(() => {});
      await elapse(0);
      expect(feeds, why).toEqual([feed]);
      upload.resources.discard();
    }
  } finally {
    Object.assign(globalThis, { WebSocket: RealWebSocket });
  }
});
