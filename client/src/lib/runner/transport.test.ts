import { afterEach, expect, jest, test } from "bun:test";
import { stubGlobals } from "../test-helpers.testutil";
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
import type { ServerStage } from "./transport";

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
async function until(predicate: () => boolean, ms = 200): Promise<void> {
  for (let i = 0; i < ms && !predicate(); i++) await Bun.sleep(1);
  expect(predicate()).toBe(true);
}

class FakeWorker {
  static all: FakeWorker[] = [];
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  readonly kind: string;
  readonly sent: { type: string; url?: string; seq?: number }[] = [];
  terminated = false;
  constructor(url: URL) {
    this.kind =
      /(download|upload|wt-transfer|ping)-worker/.exec(String(url))?.[1] ??
      "other";
    FakeWorker.all.push(this);
  }
  postMessage(message: { type: string; url?: string }): void {
    this.sent.push(message);
    if (this.kind === "wt-transfer" && message.type === "start")
      queueMicrotask(() => this.emit({ type: "established" }));
    if (this.kind === "wt-transfer" && message.type === "stop")
      queueMicrotask(() => this.emit({ type: "stopped" }));
  }
  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
  terminate(): void {
    this.terminated = true;
  }
}
const workers = (kind: string) =>
  FakeWorker.all.filter((worker) => worker.kind === kind);

interface Feed {
  signal: AbortSignal;
  write(record: object): boolean;
  terminal: { bytes: number; nanos: number };
}
let restore = () => {};
afterEach(() => restore());

/** An HTTP server: minted upload ids, NDJSON progress feeds and checkpoints. */
async function http(options: { checkpoint?: () => Response } = {}) {
  FakeWorker.all = [];
  const mints: {
    signal: AbortSignal;
    resolve: (response: Response) => void;
  }[] = [];
  const feeds = new Map<string, Feed>();
  const deleted: string[] = [];
  restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    Worker: FakeWorker,
    fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input));
      if (url.pathname === "/upload/session")
        return new Promise<Response>((resolve) =>
          mints.push({ signal: init!.signal!, resolve }),
        );
      if (url.pathname === "/upload/checkpoint")
        return options.checkpoint?.() ?? new Response(null, { status: 503 });
      const id = url.searchParams.get("id")!;
      if (init?.method === "DELETE") {
        deleted.push(id);
        feeds.get(id)?.write({ type: "complete", ...feeds.get(id)!.terminal });
        return new Response(null, { status: 204 });
      }
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
  const host = testParticipantHost(config, {
    receiver: (checkpoint) => receivers.push(checkpoint),
    fail: (_reason, message) => failures.push(message),
    stall: (info) => stalls.push(info.recoveryCause ?? info.detail),
  });
  const stage = (phase: PhaseActivity) =>
    new ServerStage({
      host,
      paths: testPreparedPaths({ latency: null }),
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
    async open(index: number, id: string) {
      await until(() => mints.length > index);
      mints[index].resolve(Response.json({ uploadId: id }));
      await until(() => feeds.has(id));
      feeds.get(id)!.write({ type: "ready" });
    },
  };
}

test("HTTP upload lanes start after the receiver feed opens and report only measured receiver evidence", async () => {
  const h = await http();
  const stage = h.stage(activity("upload"));
  const preparing = stage.prepare();
  await until(() => h.mints.length === 1);
  expect(workers("upload")).toHaveLength(0);
  await h.open(0, "one");
  await preparing;
  await until(() => workers("upload").length === 1);
  expect(
    new URL(workers("upload")[0].sent[0].url!).searchParams.get("id"),
  ).toBe("one");
  const feed = h.feeds.get("one")!;
  feed.write({ type: "progress", bytes: 50, nanos: 1e8 });
  await Bun.sleep(5);
  stage.measure();
  feed.write({ type: "progress", bytes: 100, nanos: 1e9 });
  feed.write({ type: "progress", bytes: 90, nanos: 3e9 });
  feed.write({ type: "progress", bytes: 300, nanos: 2e9 });
  await until(() => h.receivers.length === 2);
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

test("upload refusals fail the stage, an unknown id stalls, and one replacement receiver takes over", async () => {
  const h = await http();
  const stage = h.stage(activity("bidirectional"));
  const preparing = stage.prepare();
  await h.open(0, "first");
  await preparing;
  stage.measure();
  h.feeds
    .get("first")!
    .write({ type: "error", code: "invalid", message: "unknown upload" });
  await until(() => h.stalls.length === 1);
  expect(h.stalls).toEqual(["unknown-upload-id"]);
  const replacing = stage.replaceUpload(new AbortController().signal);
  expect(h.feeds.get("first")!.signal.aborted).toBe(true);
  await h.open(1, "second");
  await replacing;
  h.feeds.get("second")!.write({ type: "progress", bytes: 10, nanos: 1e9 });
  await until(() => h.receivers.length === 1);
  expect(h.receivers[0].id).toBe("second");
  h.feeds.get("second")!.write({ type: "error", code: "globalFull" });
  await until(() => h.failures.length === 1);
  expect(h.failures).toEqual(["upload progress error"]);
  stage.discard();

  const refused = h.stage(activity("bidirectional"));
  const failing = refused.prepare().catch((cause: Error) => cause.message);
  await until(() => h.mints.length === 3);
  h.mints[2].resolve(new Response(null, { status: 500 }));
  expect(await failing).toBe("upload session could not be established");
  expect(workers("download").length).toBeGreaterThan(0);
  refused.discard();
});

test("a quiet feed is backed by same-receiver checkpoints", async () => {
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
    requestedAtMs: expect.any(Number),
  });
  expect(h.receivers[1].bytes).toBeGreaterThan(h.receivers[0].bytes);
  const fresh = await stage.checkpoint(new AbortController().signal, true);
  expect(fresh?.id).toBe("quiet");
  stage.discard();
});

test("a lane error stalls once and restarts after backoff, a refusal fails the stage, and silence stalls a direction", async () => {
  const h = await http();
  jest.useFakeTimers();
  try {
    const stage = h.stage(activity("download"));
    await stage.prepare();
    jest.advanceTimersByTime(1);
    const [lane] = workers("download");
    lane.emit({ type: "progress", bytes: 100, elapsedMs: 50, seq: 0 });
    stage.measure();
    expect(lane.sent.at(-1)).toEqual({ type: "measure", seq: 1 });
    lane.emit({ type: "error", recoverable: true, detail: "reset" });
    lane.emit({ type: "error", recoverable: true, detail: "reset again" });
    expect(lane.terminated).toBe(true);
    expect(h.stalls).toEqual(["reset"]);
    jest.advanceTimersByTime(300);
    const restarted = workers("download")[1];
    expect(restarted.sent.at(-1)).toEqual({ type: "measure", seq: 1 });
    restarted.emit({ type: "error", recoverable: false, detail: "HTTP 429" });
    expect(h.failures).toEqual(["down stream 0 failed: HTTP 429"]);
    stage.discard();

    const quiet = h.stage(activity("download"));
    await quiet.prepare();
    jest.advanceTimersByTime(1);
    quiet.measure();
    jest.advanceTimersByTime(1_600);
    expect(h.stalls.at(-1)).toBe("down direction carried no data");
    quiet.discard();
  } finally {
    jest.useRealTimers();
  }
});

test("WebTransport sessions carry download bytes and relay the upload receiver feed", async () => {
  FakeWorker.all = [];
  restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    WebTransport: class {},
    Worker: FakeWorker,
    location: new URL(`${TEST_WT_ORIGIN}/`),
    fetch: async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/preflight")) return Response.json(TEST_WT_PREFLIGHT);
      if (url.includes("/upload/session"))
        return Response.json({ uploadId: "gmu_test" });
      throw new Error(`unexpected fetch ${url}`);
    },
  });
  const { ServerStage } = await import("./transport");
  const { classifyTransportDiscovery } = await import("./paths");
  const config = testWtConfig();
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
  const downloads: number[] = [];
  const receivers: ReceiverCheckpoint[] = [];
  const host = testParticipantHost(config, {
    download: (bytes) => downloads.push(bytes),
    receiver: (checkpoint) => receivers.push(checkpoint),
  });
  const create = (phase: PhaseActivity): ServerStage =>
    new ServerStage({
      host,
      paths,
      activity: phase,
      streams: { down: 4, up: 4 },
      seed: "t",
    });
  const down = create(activity("download"));
  await down.prepare();
  await until(() => workers("wt-transfer").length === 1);
  const session = () => workers("wt-transfer").at(-1)!;
  expect(session().sent[0].url).toBe(
    `${TEST_WT_ORIGIN}/wt/download?bytes=68719476736&streams=4`,
  );
  session().emit({ type: "progress", bytes: 999, elapsedMs: 10, seq: 0 });
  down.measure();
  session().emit({ type: "progress", bytes: 4_000_000, elapsedMs: 50, seq: 1 });
  await down.finish();
  expect(downloads).toEqual([4_000_000]);
  expect(session().sent.map((message) => message.type)).toContain("stop");

  const up = create(activity("upload"));
  const preparing = up.prepare();
  await until(() => workers("wt-transfer").length === 2);
  expect(session().sent[0].url).toBe(`${TEST_WT_ORIGIN}/wt/upload?id=gmu_test`);
  session().emit({ type: "upload-progress", msg: { type: "open" } });
  await preparing;
  up.measure();
  for (const n of [100, 250])
    session().emit({
      type: "upload-progress",
      msg: { type: "bytes", n, t: n * 1e6 },
    });
  expect(receivers.map((checkpoint) => checkpoint.bytes)).toEqual([100, 250]);
  up.discard();
});

test("the HTTP receiver feed reconnects without regressing counters and classifies refusals once", async () => {
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
  await until(() => events.some((event) => event.type === "complete"));
  expect(events.filter((event) => "n" in event)).toEqual([
    { type: "bytes", n: 800, t: 4 },
    { type: "complete", n: 900, t: 6 },
  ]);
  feed.dispose();
  restore();

  for (const [status, headers, expected] of [
    [403, { "Graphite-Meter-Auth": "required" }, { type: "auth-required" }],
    [
      409,
      { "X-Graphite-Upload-Refusal": "ownerMismatch" },
      { type: "fatal", cause: "owner-mismatch" },
    ],
    [503, {}, { type: "fatal", cause: "capacity-refusal" }],
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
    await until(() => seen.length > 0);
    await Bun.sleep(5);
    expect(seen).toEqual([expect.objectContaining(expected)]);
    expect(calls).toBe(1);
    refused.dispose();
    restore();
  }
});
