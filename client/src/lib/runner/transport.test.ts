import { afterEach, expect, jest, test } from "bun:test";
import { settle, stubGlobals, until } from "../test-helpers.testutil";
import { DEFAULT_CONFIG } from "../state/defaults";
import type { PhaseActivity, ReceiverCheckpoint } from "./contract";
import {
  TEST_BUILD_TOKENS,
  TEST_WT_ORIGIN,
  TEST_WT_PREFLIGHT,
  testParticipantHost,
  testPreparedPaths,
  testWtConfig,
} from "./test-helpers.testutil";
import { testWorkers, type TestWorker } from "./real/test-helpers.testutil";
import {
  PROGRESS_FINAL_GRACE_MS,
  STOP_GRACE_MS,
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
} from "./real/budgets";

const activity = (
  stage: "download" | "upload" | "bidirectional",
): PhaseActivity => ({
  stage,
  transfer:
    stage === "download"
      ? ["down"]
      : stage === "upload"
        ? ["up"]
        : ["down", "up"],
  loadedLatency: false,
});

/** A fetch lane is named by its direction; a WebTransport session establishes and acknowledges its stop. */
const kind = (worker: TestWorker): string => {
  const [, script] = /(fetch|wt-transfer|ping)-worker/.exec(worker.url) ?? [];
  if (script !== "fetch") return script ?? "other";
  return worker.sent[0]?.dir === "down" ? "download" : "upload";
};
let lanes = testWorkers();
const workers = (name: string) =>
  lanes.all.filter((worker) => kind(worker) === name);
const sessionWorkers = () =>
  testWorkers((worker, message) => {
    if (kind(worker) !== "wt-transfer") return;
    if (message.type === "start")
      queueMicrotask(() => worker.emit({ type: "established" }));
    if (message.type === "stop")
      queueMicrotask(() => worker.emit({ type: "stopped" }));
  });

interface Feed {
  signal: AbortSignal;
  write(record: object): boolean;
  terminal: { bytes: number; nanos: number };
}
let restore = () => {};
afterEach(() => {
  jest.useRealTimers();
  restore();
});

/** An HTTP server: minted upload ids, NDJSON progress feeds and checkpoints. */
async function http(
  options: {
    checkpoint?: (signal: AbortSignal) => Response | Promise<Response>;
    feed?: () => Response;
  } = {},
) {
  lanes = sessionWorkers();
  const mints: {
    signal: AbortSignal;
    resolve: (response: Response) => void;
  }[] = [];
  const feeds = new Map<string, Feed>();
  const deleted: string[] = [];
  restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    Worker: lanes.Worker,
    fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input));
      if (url.pathname === "/upload/session")
        return new Promise<Response>((resolve) =>
          mints.push({ signal: init!.signal!, resolve }),
        );
      if (url.pathname === "/upload/checkpoint")
        return (
          options.checkpoint?.(init!.signal!) ??
          new Response(null, { status: 503 })
        );
      const id = url.searchParams.get("id")!;
      if (init?.method === "DELETE") {
        deleted.push(id);
        feeds.get(id)?.write({ type: "complete", ...feeds.get(id)!.terminal });
        return new Response(null, { status: 204 });
      }
      if (options.feed) return options.feed();
      let writer!: ReadableStreamDefaultController<Uint8Array>;
      const signal = init!.signal!;
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          writer = controller;
          signal.addEventListener(
            "abort",
            () => controller.error(signal.reason),
            { once: true },
          );
        },
      });
      feeds.set(id, {
        signal,
        terminal: { bytes: 0, nanos: 0 },
        write(record) {
          if (signal.aborted) return false;
          writer.enqueue(
            new TextEncoder().encode(JSON.stringify(record) + "\n"),
          );
          return true;
        },
      });
      return new Response(body);
    },
  });
  const { ServerStage } = await import("./transport");
  const config = structuredClone(DEFAULT_CONFIG);
  config.duration.warmupMs = 0;
  const receivers: ReceiverCheckpoint[] = [];
  const failures: string[] = [];
  const stalls: (string | undefined)[] = [];
  const downloads: number[] = [];
  const hints: number[] = [];
  const host = testParticipantHost(config, {
    now: () => 42,
    download: (bytes) => downloads.push(bytes),
    uploadHint: (_lane, bytes) => hints.push(bytes),
    receiver: (checkpoint) => receivers.push(checkpoint),
    fail: (_reason, message) => failures.push(message),
    stall: (info) => stalls.push(info.rotate ? "rotate" : info.detail),
  });
  const stage = (
    phase: PhaseActivity,
    paths = testPreparedPaths({ latency: null }),
  ) =>
    new ServerStage({
      host,
      paths,
      activity: phase,
      streams: { down: 1, up: 1 },
      seed: "t",
    });
  return {
    stage,
    mints,
    feeds,
    deleted,
    receivers,
    failures,
    stalls,
    downloads,
    hints,
    async open(index: number, id: string) {
      await until(() => mints.length > index);
      mints[index].resolve(Response.json({ uploadId: id }));
      await until(() => feeds.has(id));
      feeds.get(id)!.write({ type: "ready" });
    },
  };
}

test("HTTP upload lanes start after the feed opens and count only receiver evidence", async () => {
  const h = await http();
  const stage = h.stage(activity("upload"));
  const preparing = stage.prepare();
  await until(() => h.mints.length === 1);
  expect(workers("upload")).toHaveLength(0);
  await h.open(0, "one");
  await preparing;
  await until(() => workers("upload").length === 1);
  expect(
    new URL(String(workers("upload")[0].sent[0].url)).searchParams.get("id"),
  ).toBe("one");
  const feed = h.feeds.get("one")!;
  feed.write({ type: "progress", bytes: 50, nanos: 1e8 });
  await settle();
  stage.measure();
  feed.write({ type: "progress", bytes: 100, nanos: 1e9 });
  feed.write({ type: "progress", bytes: 90, nanos: 3e9 });
  feed.write({ type: "progress", bytes: 300, nanos: 2e9 });
  workers("upload")[0].emit({ type: "alive", bytes: 5_000, elapsedMs: 10 });
  await until(() => h.receivers.length === 2);
  expect(h.hints).toEqual([5_000]);
  expect(h.downloads).toEqual([]);
  expect(
    h.receivers.map(({ id, bytes, nanos }) => ({ id, bytes, nanos })),
  ).toEqual([
    { id: "one", bytes: 100, nanos: 1e9 },
    { id: "one", bytes: 300, nanos: 2e9 },
  ]);
  feed.terminal = { bytes: 400, nanos: 2.5e9 };
  await stage.finish();
  expect(h.deleted).toEqual(["one"]);
  expect(h.failures).toEqual([]);
});

test("a late mint from a discarded stage cannot adopt resources", async () => {
  const h = await http();
  const old = h.stage(activity("upload"));
  const preparing = old.prepare().catch((cause: Error) => cause.message);
  await until(() => h.mints.length === 1);
  old.discard();
  expect(h.mints[0].signal.aborted).toBe(true);
  h.mints[0].resolve(Response.json({ uploadId: "old" }));
  await preparing;
  expect(h.feeds.size).toBe(0);
  expect(workers("upload")).toHaveLength(0);
});

test("upload refusals fail the stage, an unknown id stalls even mid-recovery, and one replacement receiver takes over", async () => {
  const h = await http();
  const stage = h.stage(activity("bidirectional"));
  const preparing = stage.prepare();
  await h.open(0, "first");
  await preparing;
  stage.measure();
  await until(() => workers("upload").length === 1);
  workers("upload")[0].emit({
    type: "error",
    reason: "connection-lost",
    retry: true,
    detail: "reset",
  });
  h.feeds
    .get("first")!
    .write({ type: "error", code: "invalid", message: "unknown upload" });
  await until(() => h.stalls.length === 2);
  expect(h.stalls).toEqual(["reset", "rotate"]);
  const replacing = stage.replaceUpload(new AbortController().signal);
  expect(h.feeds.get("first")!.signal.aborted).toBe(true);
  await h.open(1, "second");
  await replacing;
  h.feeds.get("second")!.write({ type: "progress", bytes: 10, nanos: 1e9 });
  await until(() => h.receivers.length === 1);
  expect(h.receivers[0].id).toBe("second");
  h.feeds.get("second")!.write({ type: "error", code: "ownerMismatch" });
  await until(() => h.failures.length === 1);
  expect(h.failures).toEqual(["upload progress error"]);
  stage.discard();

  const revoked = h.stage(activity("upload"));
  const preparingRevoked = revoked.prepare();
  await h.open(2, "third");
  await preparingRevoked;
  revoked.measure();
  h.feeds.get("third")!.write({ type: "error", code: "revoked" });
  await until(() => h.failures.length === 2);
  expect(h.failures[1]).toBe("Sign in again to measure throughput");

  const refused = h.stage(activity("bidirectional"));
  const failing = refused.prepare().catch((cause: Error) => cause.message);
  await until(() => h.mints.length === 4);
  h.mints[3].resolve(new Response(null, { status: 500 }));
  expect(await failing).toBe("upload session could not be established");
  expect(workers("download").length).toBeGreaterThan(0);
  refused.discard();
});

test("a quiet feed is backed by same-receiver checkpoints", async () => {
  jest.useFakeTimers();
  let bytes = 0;
  const h = await http({
    checkpoint: () =>
      Response.json({ bytes: (bytes += 100), nanos: bytes * 1e6 }),
  });
  const stage = h.stage(activity("upload"));
  const preparing = stage.prepare();
  await h.open(0, "quiet");
  await preparing;
  stage.measure();
  await until(() => h.receivers.length >= 2, 2_000);
  expect(h.receivers[0]).toMatchObject({
    id: "quiet",
    requestedAtMs: 42,
    receivedAtMs: 42,
  });
  expect(h.receivers[1].bytes).toBeGreaterThan(h.receivers[0].bytes);
  const fresh = await stage.checkpoint(new AbortController().signal, true);
  expect(fresh?.id).toBe("quiet");
  stage.discard();
});

test("a lane error stalls once and restarts after backoff, and a refusal fails the stage", async () => {
  const h = await http();
  jest.useFakeTimers();
  try {
    const stage = h.stage(activity("download"));
    const preparing = stage.prepare();
    // Without a warmup stagger the worker loads before anything else can run.
    expect(workers("download")).toHaveLength(1);
    await preparing;
    const [lane] = workers("download");
    lane.emit({ type: "progress", bytes: 100, elapsedMs: 50, seq: 0 });
    stage.measure();
    expect(lane.sent.at(-1)).toEqual({ type: "measure", seq: 1 });
    const lost = { type: "error", reason: "connection-lost", retry: true };
    lane.emit({ ...lost, detail: "reset" });
    lane.emit({ ...lost, detail: "reset again" });
    expect(lane.terminated).toBe(1);
    expect(h.stalls).toEqual(["reset"]);
    jest.advanceTimersByTime(300);
    const restarted = workers("download")[1];
    expect(restarted.sent.at(-1)).toEqual({ type: "measure", seq: 1 });
    restarted.emit({
      type: "error",
      reason: "protocol-error",
      retry: false,
      detail: "HTTP 400",
    });
    expect(h.failures).toEqual(["down stream 0 failed: HTTP 400"]);
    stage.discard();
  } finally {
    jest.useRealTimers();
  }
});

test("a busy WebTransport upload session restarts once, then names the server busy", async () => {
  lanes = sessionWorkers();
  restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    WebTransport: class {},
    Worker: lanes.Worker,
    location: new URL(`${TEST_WT_ORIGIN}/`),
    fetch: async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/upload/session"))
        return Response.json({ uploadId: "gmu_test" });
      throw new Error(`unexpected fetch ${url}`);
    },
  });
  const { ServerStage } = await import("./transport");
  const { classifyTransportDiscovery } = await import("./paths");
  type Advertised = Parameters<typeof classifyTransportDiscovery>;
  const discovery = classifyTransportDiscovery(
    TEST_WT_PREFLIGHT.capabilities.throughput as Advertised[0],
    TEST_WT_PREFLIGHT.capabilities.latency as Advertised[1],
    TEST_WT_ORIGIN,
    true,
  );
  const target = discovery.throughput[TEST_WT_ORIGIN].targets[0];
  const paths = testPreparedPaths({ latency: null });
  paths.throughput = { ...paths.throughput, requested: target, target };
  paths.throughput.fetch = {
    ...paths.throughput.fetch,
    origin: TEST_WT_ORIGIN,
  };
  jest.useFakeTimers();
  const refused = new ServerStage({
    host: testParticipantHost(testWtConfig()),
    paths,
    activity: activity("upload"),
    streams: { down: 4, up: 4 },
    seed: "t",
  });
  const refusing = refused
    .prepare()
    .catch((cause: Error) => cause.constructor.name);
  await until(() => workers("wt-transfer").length === 1);
  const [first] = workers("wt-transfer");
  expect(first.sent[0].url).toBe(`${TEST_WT_ORIGIN}/wt/upload?id=gmu_test`);
  first.emit({
    type: "upload-progress",
    msg: {
      type: "fatal",
      detail: "client upload capacity exhausted",
      reason: "server-busy",
      retry: true,
    },
  });
  expect(first.terminated).toBe(1);
  jest.advanceTimersByTime(300);
  expect(workers("wt-transfer")).toHaveLength(2);
  await settle();
  jest.advanceTimersByTime(3_200);
  expect(await refusing).toBe("ServerBusyError");
  refused.discard();
});

test("the HTTP receiver feed keeps its counters, retries a busy server and classifies refusals once", async () => {
  jest.useFakeTimers();
  const { uploadFeed } = await import("./transport");
  const events: Parameters<Parameters<typeof uploadFeed>[0]["onEvent"]>[0][] =
    [];
  let gets = 0;
  const bodies = [
    '{"type":"rea',
    'dy"}\n{"type":"progress","bytes":800,"nanos":4}\n',
    '{"type":"ready"}\n{"type":"progress","bytes":300,"nanos":5}\n{"type":"complete","bytes":900,"nanos":6}\n',
  ];
  restore = stubGlobals({
    fetch: async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === "DELETE") return new Response(null, { status: 204 });
      gets++;
      return new Response(gets === 1 ? bodies[0] + bodies[1] : bodies[2]);
    },
  });
  const feed = uploadFeed({
    url: "https://meter.test/upload/progress?id=one",
    csrf: {},
    credentials: "same-origin",
    onEvent: (event) => events.push(event),
  });
  await until(() => events.some((event) => event.type === "complete"), 2_000);
  expect(events.filter((event) => "n" in event)).toEqual([
    { type: "bytes", n: 800, t: 4 },
    { type: "complete", n: 900, t: 6 },
  ]);
  feed.dispose();
  restore();

  for (const [status, headers, expected, retried] of [
    [
      403,
      { "Graphite-Meter-Auth": "required" },
      { type: "auth-required" },
      false,
    ],
    [
      409,
      { "X-Graphite-Upload-Refusal": "ownerMismatch" },
      { type: "fatal", reason: "protocol-error" },
      false,
    ],
    [503, {}, { type: "stall", reason: "server-busy" }, true],
  ] as const) {
    let calls = 0;
    const seen: object[] = [];
    restore = stubGlobals({
      fetch: async () => {
        calls++;
        return new Response(null, { status, headers: { ...headers } });
      },
    });
    const refused = uploadFeed({
      url: "https://meter.test/p",
      csrf: {},
      credentials: "omit",
      onEvent: (e) => seen.push(e),
    });
    await until(() => seen.length > 0, 2_000);
    jest.advanceTimersByTime(5_000);
    if (retried) await until(() => calls > 1, 2_000);
    expect(seen.slice(0, 1)).toEqual([expect.objectContaining(expected)]);
    expect(calls > 1).toBe(retried);
    refused.dispose();
    restore();
  }
});

test("a busy lane reconnects with a doubling, capped delay, and lapsed readiness names the server busy", async () => {
  const h = await http();
  jest.useFakeTimers();
  try {
    const stage = h.stage(activity("download"));
    await stage.prepare();
    const owner = new AbortController();
    const waiting = stage.ready(owner.signal).catch((cause) => cause);
    const busy = { type: "error", reason: "server-busy", retry: true };
    for (const [retryAfterMs, delayMs] of [
      [undefined, 300],
      [undefined, 600],
      [1_000, 1_200],
      [5_000, 1_200],
    ] as const) {
      const count = workers("download").length;
      workers("download")
        .at(-1)!
        .emit({ ...busy, retryAfterMs, detail: "HTTP 429" });
      jest.advanceTimersByTime(delayMs - 1);
      expect(workers("download")).toHaveLength(count);
      jest.advanceTimersByTime(1);
      expect(workers("download")).toHaveLength(count + 1);
    }
    expect(h.failures).toEqual([]);
    owner.abort();
    expect((await waiting).constructor.name).toBe("ServerBusyError");
    stage.discard();
  } finally {
    jest.useRealTimers();
  }
});

test("a busy upload feed during preparation retries with the busy backoff, then names the server busy", async () => {
  let gets = 0;
  const h = await http({
    feed: () => {
      gets++;
      return new Response(null, {
        status: 429,
        headers: { "Retry-After": "1" },
      });
    },
  });
  jest.useFakeTimers();
  const stage = h.stage(activity("upload"));
  const preparing = stage
    .prepare()
    .catch((cause: Error) => cause.constructor.name);
  await until(() => h.mints.length === 1);
  h.mints[0].resolve(Response.json({ uploadId: "busy" }));
  await until(() => gets === 1);
  await settle();
  jest.advanceTimersByTime(999);
  await settle();
  expect(gets).toBe(1);
  jest.advanceTimersByTime(1);
  await until(() => gets === 2);
  jest.advanceTimersByTime(3_500);
  expect(await preparing).toBe("ServerBusyError");
  expect(h.failures).toEqual([]);
  stage.discard();
});

test("loaded latency that never answers leaves throughput ready; the latency stage still fails", async () => {
  const h = await http();
  jest.useFakeTimers();
  const loaded = h.stage(
    { ...activity("download"), loadedLatency: true },
    testPreparedPaths(),
  );
  await loaded.prepare();
  const ready = loaded.ready(new AbortController().signal);
  workers("download")[0].emit({ type: "progress", bytes: 10 });
  await settle();
  jest.advanceTimersByTime(3_500);
  await ready;
  expect(h.failures).toEqual([]);
  loaded.discard();

  const idle = h.stage(
    { stage: "latency", transfer: [], loadedLatency: false },
    testPreparedPaths(),
  );
  await idle.prepare();
  const waiting = idle
    .ready(new AbortController().signal)
    .catch((cause: Error) => cause.message);
  jest.advanceTimersByTime(3_500);
  expect(await waiting).toBe(
    "Primed measurement connections did not become ready",
  );
  idle.discard();
});

test("stage readiness wakes on the first bytes of every download lane", async () => {
  const server = await http();
  const stage = server.stage(activity("download"));
  await stage.prepare();
  let ready = false;
  const waiting = stage.ready(new AbortController().signal);
  void waiting.then(() => (ready = true));
  const [lane] = workers("download");
  lane.emit({ type: "progress", bytes: 0 });
  await settle();
  expect(ready).toBe(false);
  lane.emit({ type: "progress", bytes: 10 });
  await waiting;
  stage.discard();
});

test("a session lane times out its establishment and is released when it never acknowledges stop", async () => {
  jest.useFakeTimers();
  const { openLane } = await import("./transport");
  const failures: object[] = [];
  const open = (worker: TestWorker) =>
    openLane(worker as unknown as Worker, {}, true, (msg) => {
      if (msg.type === "error") failures.push(msg);
    });
  const established = new (testWorkers().Worker)("wt-transfer-worker");
  open(established);
  established.emit({ type: "established" });
  const silent = new (testWorkers().Worker)("wt-transfer-worker");
  const lane = open(silent);
  jest.advanceTimersByTime(ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS - 1);
  expect(failures).toEqual([]);
  jest.advanceTimersByTime(1);
  expect(failures).toEqual([
    {
      type: "error",
      detail: "webtransport session did not establish",
      reason: "timeout",
      retry: true,
    },
  ]);

  let released = false;
  const stopping = lane.stop().then(() => (released = true));
  expect(silent.sent.at(-1)).toEqual({ type: "stop" });
  jest.advanceTimersByTime(STOP_GRACE_MS - 1);
  await settle();
  expect([released, silent.terminated]).toEqual([false, 0]);
  jest.advanceTimersByTime(1);
  await stopping;
  expect(silent.terminated).toBe(1);
});

test("an HTTP receiver feed without its final record releases the stage after the final grace", async () => {
  const h = await http();
  const stage = h.stage(activity("upload"));
  const preparing = stage.prepare();
  await h.open(0, "one");
  await preparing;
  stage.measure();
  h.feeds.delete("one");
  jest.useFakeTimers();
  let finished = false;
  const finishing = stage.finish().then(() => (finished = true));
  await settle();
  expect(h.deleted).toEqual(["one"]);
  jest.advanceTimersByTime(PROGRESS_FINAL_GRACE_MS - 1);
  await settle();
  expect(finished).toBe(false);
  jest.advanceTimersByTime(1);
  await finishing;
});

test("a checkpoint the server never answers gives up on its own bound", async () => {
  const h = await http({
    checkpoint: (signal) =>
      new Promise((_, reject) =>
        signal.addEventListener("abort", () => reject(signal.reason), {
          once: true,
        }),
      ),
  });
  const stage = h.stage(activity("upload"));
  const preparing = stage.prepare();
  await h.open(0, "hung");
  await preparing;
  jest.useFakeTimers();
  let outcome: unknown = "pending";
  void stage.checkpoint(new AbortController().signal, true).then(
    (value) => (outcome = value),
    (cause: Error) => (outcome = cause.name),
  );
  jest.advanceTimersByTime(1_499);
  await settle();
  expect(outcome).toBe("pending");
  jest.advanceTimersByTime(1);
  await until(() => outcome !== "pending");
  expect(outcome).toBe("TimeoutError");
  stage.discard();
});
