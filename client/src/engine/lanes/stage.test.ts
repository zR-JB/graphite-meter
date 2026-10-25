import { expect, jest, test } from "bun:test";
import { handshakes, THROTTLED } from "../net/webtransport";
import type { Activity } from "../run/schedule";
import type {
  Discovery,
  FetchTarget,
  LatencyPath,
  LatencyTarget,
  Link,
  PreparedServer,
  ProbeChannelEvents,
  ServerAuth,
  SessionTarget,
  StageEvents,
  StageSetup,
} from "../types";
import type { Lease } from "../net/lease";
import { elapse, FakeWorker, outcome, useFakeWorkers } from "./fakes.testutil";
import { sessionClosed } from "./endings";
import { createWorkerPool } from "./pool";
import { createBusSlot, openProbeChannel } from "./probes";
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
const LATENCY: LatencyTarget = {
  id: ORIGIN,
  origin: ORIGIN,
  transport: "websocket",
  protocol: "http1",
  tls: true,
};
const IGNORED = new Proxy({} as ProbeChannelEvents, { get: () => () => {} });
/** A server's link whose lease mints upload ids with `mint`. */
const testLink = (mint: () => Promise<string> = async () => "u1"): Link => ({
  bus: createBusSlot(openProbeChannel, IGNORED),
  workers: createWorkerPool(),
  lease: { upload: mint } as unknown as Lease,
});
/** A discovery origin over HTTP/1.1 offering the bus's WebSocket. */
const DISCOVERY = {
  origin: ORIGIN,
  protocol: "http/1.1",
  latency: new Map([[ORIGIN, { blocked: null, targets: [{ transport: "websocket" }] }]]),
} as unknown as Discovery;
const DOWNLOAD: Activity = { stage: "download", transfer: ["down"], loadedLatency: false };

/** One server's stage over `target`, with `auth` answering its credentials, `link` as its server's and `server` overriding the rest. */
function stage(
  activity: Activity,
  auth: Partial<ServerAuth> = {},
  target: FetchTarget | SessionTarget = FETCH,
  link: Link = testLink(),
  server: Partial<PreparedServer> = {},
  uploadId: StageSetup["uploadId"] = Promise.resolve(null),
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
    socket: (url) => url,
    ended: async () => (checks++, false),
    ...auth,
  };
  const resources = openStage({
    server: {
      server: { id: "peer", url: ORIGIN, name: "Peer" },
      auth: credentials,
      throughput: { target, fetch: target.transport === "fetch-stream" ? target : FETCH },
      latency: { target: LATENCY },
      discovery: DISCOVERY,
      link,
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
    events: new Proxy({} as StageEvents, {
      get:
        (_, type: keyof StageEvents) =>
        (...args: unknown[]) =>
          events.push([type, ...args]),
    }),
    now: () => performance.now(),
    endsIn: () => 6_000,
    uploadId,
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

const stopped = (worker: FakeWorker) => worker.commands.at(-1)?.type === "stop";
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
  expect(refused.lanes().every(stopped)).toBe(true);

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
    reason: "path-failed",
    message: "Connections did not become ready within 3.5 s",
  });
});

test("a lane refused at its client limit is asked again once, after 300 ms, and its second refusal fails the stage so", async () => {
  const limited = stage(DOWNLOAD);
  const prepared = outcome(limited.resources.prepare(new AbortController().signal));
  const [lane] = limited.lanes();
  const opened: number[] = [];
  for (let at = 0; at < 3_500; at += 5) {
    if (lane!.commands.filter(({ type }) => type === "download").length > opened.length) {
      opened.push(at);
      lane!.report({
        type: "ended",
        ending: { reason: "client-limit", retry: true, detail: "HTTP 429" },
      });
    }
    await elapse(5);
  }
  expect(opened).toEqual([0, 300]);
  expect(prepared.error).toMatchObject({
    reason: "client-limit",
    message: "down stream 0 failed: HTTP 429",
  });
});

test("a lane that reached the HTTP/3 port over TCP fails the stage at once, without its reopen", async () => {
  const fallen = stage(DOWNLOAD);
  const prepared = outcome(fallen.resources.prepare(new AbortController().signal));
  const [lane] = fallen.lanes();
  const detail = "this port serves transfers over HTTP/3, not TCP";
  lane!.report({ type: "ended", ending: { reason: "path-failed", retry: false, detail } });
  await elapse(400);
  expect(lane!.commands.filter(({ type }) => type === "download")).toHaveLength(1);
  expect(prepared.error).toMatchObject({
    reason: "path-failed",
    message: `down stream 0 failed: ${detail}`,
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
  expect(lanes.every(stopped)).toBe(true);
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
    expect(lanes.every(stopped)).toBe(signedOut);
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

test("a session's stop waits for its worker's acknowledgement, at most 3.25 s", async () => {
  for (const acknowledged of [true, false]) {
    const session = stage(DOWNLOAD, {}, SESSION);
    void session.resources.prepare(new AbortController().signal);
    const worker = session.session();
    expect(worker.commands).toEqual([
      {
        type: "download",
        url: `${ORIGIN}/wt/download?bytes=${64 * 1024 ** 3}&streams=3`,
        datagrams: false,
        epoch: 0,
      },
    ]);
    bytes(worker, 0);
    await elapse(0);
    const finished = outcome(session.resources.finish());
    expect(worker.commands.at(-1)).toEqual({ type: "stop" });
    if (acknowledged) worker.report({ type: "stopped" });
    await elapse(3_500);
    expect(finished.at).toBe(acknowledged ? 0 : 3_250);
    expect(worker.terminated).toBe(!acknowledged);
  }
});

test("a broken session redials once its server answered over HTTP; one that does not answer within 3 s, or whose redial does not open, fails the stage", async () => {
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
    if (answering) {
      worker.report({
        type: "ended",
        ending: {
          reason: "path-failed",
          retry: true,
          detail: "webtransport session did not establish",
        },
      });
      expect(session.events.filter(([type]) => type === "failed")).toEqual([
        [
          "failed",
          "connection-lost",
          "down stream 0 failed: webtransport session did not establish",
        ],
      ]);
    }
    session.resources.discard();
  }
});

test("a session whose dial and redial do not open fails the stage's preparation as its path", async () => {
  const session = stage(DOWNLOAD, {}, SESSION);
  const prepared = outcome(session.resources.prepare(new AbortController().signal));
  const worker = session.session();
  const unopened = {
    type: "ended",
    ending: {
      reason: "path-failed",
      retry: true,
      detail: "webtransport session did not establish",
    },
  } as const;
  worker.report(unopened);
  await elapse(300);
  worker.report(unopened);
  await elapse(0);
  expect(prepared.error).toMatchObject({
    reason: "path-failed",
    message: "down stream 0 failed: webtransport session did not establish",
  });
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
      ["failed", "path-failed", THROTTLED],
    ]);
    // A later run over the same path, which its break left verified, dials nothing either.
    const later = stage(DOWNLOAD, {}, SESSION);
    const refused = outcome(later.resources.prepare(new AbortController().signal));
    await elapse(0);
    expect(later.session().commands.filter(({ type }) => type === "download")).toHaveLength(0);
    expect(refused.error).toMatchObject({ reason: "path-failed", message: THROTTLED });
    const latency = { target: { ...SESSION, transport: "webtransport" } } as unknown as LatencyPath;
    const loaded = stage({ ...DOWNLOAD, loadedLatency: true }, {}, FETCH, testLink(), { latency });
    outcome(loaded.resources.prepare(new AbortController().signal));
    expect([loaded.lanes().length, loaded.probes().length]).toEqual([3, 0]);
    expect(loaded.events.filter(([type]) => type === "latencyStalled")).toEqual([
      ["latencyStalled", THROTTLED],
    ]);
    const idle = stage(
      { stage: "latency", transfer: [], loadedLatency: false },
      {},
      FETCH,
      testLink(),
      { latency },
    );
    const prepared = outcome(idle.resources.prepare(new AbortController().signal));
    await elapse(0);
    expect(prepared.error).toMatchObject({ reason: "path-failed", message: THROTTLED });
    for (const each of [session, loaded, idle]) each.resources.discard();
  } finally {
    // The page's ledger lifts with time, for the tests after this one.
    await elapse(0);
    jest.advanceTimersByTime(5 * 60_000);
  }
});

test("a session discarded while it dials, its worker never acknowledging, counts as a failed handshake, as Chrome counts it; one that delivered does not", async () => {
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
    await elapse(3_250);
    expect(handshakes.throttled()).toBe(false);
    const fourth = stage(DOWNLOAD, {}, SESSION);
    outcome(fourth.resources.prepare(new AbortController().signal));
    fourth.resources.discard();
    await elapse(3_250);
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

test("a stage's download lanes end on the server's clock just past its planned end once the clock runs, and a stop waits at most 1 s for them, then cuts them", async () => {
  const lanes = stage(DOWNLOAD);
  void lanes.resources.prepare(new AbortController().signal);
  await elapse(150);
  const opened = lanes.lanes();
  // While the clock holds for the stage to prepare, its end is not known yet.
  expect(opened.map(({ commands }) => [commands[0]!.until, commands[0]!.timed])).toEqual(
    Array(3).fill([null, true]),
  );
  for (const lane of opened) bytes(lane, 0);
  await elapse(0);
  lanes.resources.replanned();
  // 6 s planned and a 250 ms margin.
  expect(opened.map(({ commands }) => commands.at(-1))).toEqual(
    Array(3).fill({ type: "until", until: performance.timeOrigin + performance.now() + 6_250 }),
  );
  const finished = outcome(lanes.resources.finish());
  expect(opened.map(({ commands }) => commands.at(-1))).toEqual(Array(3).fill({ type: "stop" }));
  opened[0]!.report({ type: "stopped" });
  opened[1]!.report({ type: "stopped" });
  await elapse(500);
  opened[2]!.report({ type: "stopped" });
  await elapse(0);
  expect([finished.at, opened.some(({ terminated }) => terminated)]).toEqual([500, false]);
  // HTTP/1.1 bodies keep a length, as a proxy would re-chunk one without.
  const late = stage(DOWNLOAD, {}, { ...FETCH, protocol: "http1" });
  void late.resources.prepare(new AbortController().signal);
  await elapse(150);
  expect(late.lanes().map(({ commands }) => commands[0]!.timed)).toEqual([false, false, false]);
  for (const lane of late.lanes()) bytes(lane, 0);
  await elapse(0);
  const cut = outcome(late.resources.finish());
  await elapse(1_000);
  const stops = (lane: FakeWorker) => lane.commands.filter(({ type }) => type === "stop").length;
  expect(late.lanes().map(stops)).toEqual([2, 2, 2]);
  await elapse(250);
  expect([cut.at, late.lanes().every(({ terminated }) => terminated)]).toEqual([1_250, true]);
});

test("a stage's first upload takes the run's id; its replacement is minted with the lease", async () => {
  const minted: string[] = [];
  const upload = stage(
    { stage: "upload", transfer: ["up"], loadedLatency: false },
    {},
    SESSION,
    testLink(async () => (minted.push("lease"), "u2")),
    {},
    Promise.resolve("u1"),
  );
  outcome(upload.resources.prepare(new AbortController().signal));
  await elapse(0);
  expect([minted, upload.session().commands[0]]).toMatchObject([
    [],
    { type: "upload", url: `${ORIGIN}/wt/upload?id=u1` },
  ]);
  void upload.resources.replaceUpload(new AbortController().signal);
  upload.session().report({ type: "stopped" });
  await elapse(0);
  expect(minted).toEqual(["lease"]);
  upload.resources.discard();
});

test("one latency bus serves the server's run: a later stage adopts and re-paces it, and a discard leaves it to the server", async () => {
  const link = testLink();
  const idle = stage({ stage: "latency", transfer: [], loadedLatency: false }, {}, FETCH, link);
  const prepared = outcome(idle.resources.prepare(new AbortController().signal));
  const [probe] = idle.probes();
  expect(probe!.commands).toEqual([
    {
      type: "start",
      url: "wss://peer.example/ws/ping",
      transport: "websocket",
      intervalMs: 80,
      maxInFlight: 16,
    },
    // The latency stage shows its warmup's replies.
    { type: "warm" },
  ]);
  probe!.report({ type: "ready" });
  await elapse(0);
  expect(prepared).toEqual({ at: 0 });
  idle.resources.measure(performance.now());
  const finished = outcome(idle.resources.finish());
  probe!.report({ type: "drained", id: 1 });
  await elapse(0);
  expect(finished).toEqual({ at: 0 });
  expect(probe!.terminated).toBe(false);

  // The loaded stage dials nothing, so its transfers never race a handshake.
  const loaded = stage(
    { stage: "download", transfer: ["down"], loadedLatency: true },
    {},
    FETCH,
    link,
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
  expect(probe!.terminated).toBe(false);
  expect(loaded.events.at(-1)).toEqual(["probesIncomplete"]);
  expect(link.bus.adopt(LATENCY, IGNORED)).not.toBeNull();
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
  const replaced = stage(
    UPLOAD,
    {
      fetch: async (url, init) => {
        if (url.includes("/upload/progress?id=u1")) throw new TypeError("network error");
        return pending(init);
      },
    },
    FETCH,
    testLink(async () => `u${minted.push("lease")}`),
    { discovery: { ...DISCOVERY, latency: new Map() } },
  );
  const prepared = outcome(replaced.resources.prepare(new AbortController().signal));
  await elapse(400);
  void replaced.resources.replaceUpload(new AbortController().signal);
  await elapse(100);
  expect(minted).toHaveLength(2);
  expect(prepared.error).toBeUndefined();
  replaced.resources.discard();
});

test("a fetch upload's feed reaches the discovery origin: over its WebSocket on HTTP/1.1, else over HTTP", async () => {
  const BUS = "https://bus.example";
  const TRANSFER = "https://transfer.example";
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
        "http/1.1",
        [BUS, ORIGIN],
        "wss://peer.example/ws/progress?id=u1",
      ],
      [
        "an unproven protocol, which may be multiplexed",
        null,
        [ORIGIN],
        `${ORIGIN}/upload/progress?id=u1`,
      ],
      ["HTTP/1.1 without a WebSocket", "http/1.1", [BUS], `${ORIGIN}/upload/progress?id=u1`],
      ["HTTP/2", "h2", [ORIGIN], `${ORIGIN}/upload/progress?id=u1`],
      ["HTTP/3", "h3", [ORIGIN], `${ORIGIN}/upload/progress?id=u1`],
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
            if (url.includes("/upload/progress")) feeds.push(url);
            return new Promise((_, reject) =>
              init!.signal!.addEventListener("abort", () => reject(init!.signal!.reason)),
            );
          },
        },
        { ...FETCH, id: TRANSFER, origin: TRANSFER },
        testLink(),
        {
          discovery: { origin: ORIGIN, protocol, latency: offered(...sockets) } as Discovery,
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

test("a later stage's lanes run in the workers an earlier stage stopped, and a worker that never acknowledged is replaced", async () => {
  const link = testLink();
  const first = stage(DOWNLOAD, {}, FETCH, link);
  void first.resources.prepare(new AbortController().signal);
  const lanes = first.lanes();
  for (const lane of lanes) bytes(lane, 0);
  await elapse(0);
  const finished = first.resources.finish();
  for (const lane of lanes.slice(1)) lane.report({ type: "stopped" });
  await elapse(3_250);
  await finished;
  expect(lanes.map(({ terminated }) => terminated)).toEqual([true, false, false]);
  const spawned = FakeWorker.spawned.length;
  const second = stage(DOWNLOAD, {}, FETCH, link);
  outcome(second.resources.prepare(new AbortController().signal));
  await elapse(200);
  expect(FakeWorker.spawned.length).toBe(spawned + 1);
  expect(lanes.slice(1).map(({ commands }) => commands.at(-1)?.type)).toEqual([
    "download",
    "download",
  ]);
  second.resources.discard();
});

test("a stage discarded while its bus is stalled parks it with the server, whose stall closes it", async () => {
  const link: Link = {
    ...testLink(),
    bus: createBusSlot(openProbeChannel, { ...IGNORED, stalled: () => link.bus.dispose() }),
  };
  const idle = stage({ stage: "latency", transfer: [], loadedLatency: false }, {}, FETCH, link);
  outcome(idle.resources.prepare(new AbortController().signal));
  const [probe] = idle.probes();
  probe!.report({ type: "stalled", detail: "websocket closed" });
  idle.resources.discard(true);
  expect(probe!.terminated).toBe(true);
});
