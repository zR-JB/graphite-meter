import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import type { LaneCommand, ProbeCommand, SessionCommand } from "../../protocol/messages";
import { elapse } from "../lanes/fakes.testutil";

const encoder = new TextEncoder();
const GLOBALS = [
  "onmessage",
  "postMessage",
  "fetch",
  "Response",
  "WebTransport",
  "WebSocket",
] as const;
const saved = Object.fromEntries(GLOBALS.map((name) => [name, Reflect.get(globalThis, name)]));
let realms = 0;

beforeEach(() => {
  jest.useFakeTimers();
  FakeWebTransport.dialed = [];
  FakeWebSocket.opened = [];
});
afterEach(() => {
  jest.useRealTimers();
  Object.assign(globalThis, saved);
});

/** Boots a fresh copy of a worker module against `globals`, yielding what it posts and its owner's side of the port. */
async function boot<Command>(
  module: string,
  globals: Partial<Record<(typeof GLOBALS)[number], unknown>>,
) {
  const posted: { type: string; [field: string]: unknown }[] = [];
  Object.assign(globalThis, {
    onmessage: null,
    postMessage: (report: (typeof posted)[number]) => posted.push(report),
  });
  Object.assign(globalThis, globals);
  await import(`${module}?realm=${realms++}`);
  const port = globalThis.onmessage as unknown as (event: { data: Command }) => void;
  return {
    posted,
    send: (data: Command) => port({ data }),
    types: () => posted.map(({ type }) => type),
  };
}

/** Lets every pending microtask run, and a task posted before this one. */
const task = () =>
  new Promise<void>((resolve) => {
    const { port1, port2 } = new MessageChannel();
    port1.onmessage = () => {
      port1.close();
      resolve();
    };
    port2.postMessage(null);
  });

function source() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({ start: (c) => void (controller = c) });
  return {
    stream,
    push: (text: string) => controller.enqueue(encoder.encode(text)),
    controller: () => controller,
  };
}

/** The test answers for the server on this WebTransport session, its handshake, server streams, datagrams and close. */
class FakeWebTransport {
  static dialed: FakeWebTransport[] = [];
  readonly ready: Promise<void>;
  readonly closed: Promise<WebTransportCloseInfo>;
  readonly accept: () => void;
  readonly refuse: () => void;
  /** Closes the session. The first close, ours or the server's, is the one `closed` reports. */
  readonly end: (info: WebTransportCloseInfo) => void;
  #streams!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;
  #datagrams!: ReadableStreamDefaultController<Uint8Array>;
  readonly incomingUnidirectionalStreams = new ReadableStream<ReadableStream<Uint8Array>>({
    start: (controller) => void (this.#streams = controller),
  });
  readonly datagrams = {
    maxDatagramSize: 1_200,
    readable: new ReadableStream<Uint8Array>({
      start: (controller) => void (this.#datagrams = controller),
    }),
    writable: new WritableStream<Uint8Array>(),
  };

  constructor(readonly url: string) {
    const ready = Promise.withResolvers<void>();
    const closed = Promise.withResolvers<WebTransportCloseInfo>();
    [this.ready, this.closed, this.accept, this.end] = [
      ready.promise,
      closed.promise,
      ready.resolve,
      closed.resolve,
    ];
    this.refuse = () => {
      ready.reject(new Error("Opening handshake failed."));
      closed.reject(new Error("Opening handshake failed."));
    };
    FakeWebTransport.dialed.push(this);
  }

  open(stream: ReadableStream<Uint8Array>): void {
    this.#streams.enqueue(stream);
  }

  datagram(bytes: number): void {
    this.#datagrams.enqueue(new Uint8Array(bytes));
  }

  reset(): void {
    this.#streams.error(new Error("stream reset"));
    this.#datagrams.error(new Error("datagrams reset"));
  }

  createUnidirectionalStream(): Promise<WritableStream<Uint8Array>> {
    return Promise.resolve(new WritableStream({ write: () => new Promise(() => {}) }));
  }

  close(): void {
    this.end({ closeCode: 0, reason: "" });
  }
}

const RECIPE = {
  headers: { authorization: "Bearer g" },
  credentials: "omit",
  redirect: "error",
} as const;
const REVOKED = { closeCode: 3, reason: "authentication required" };

test("a download counts bytes into the epoch they were read in, from the time its measure names", async () => {
  const body = source();
  const lane = await boot<LaneCommand>("./lane.ts", {
    fetch: async () => new Response(body.stream),
  });
  lane.send({
    type: "download",
    url: "https://peer.example/download?bytes=1",
    request: RECIPE,
    epoch: 0,
    streams: 1,
    timed: true,
    until: 0,
  });
  await elapse(0);
  body.push("warmup");
  await elapse(5);
  const at = performance.timeOrigin + performance.now();
  await elapse(5);
  // Read after the window opened, before the lane heard of it.
  body.push("early");
  await elapse(5);
  lane.send({ type: "measure", epoch: 1, at });
  await elapse(50);
  body.push("measured");
  await elapse(0);
  body.controller().error(new TypeError("network error"));
  await elapse(0);
  expect(lane.posted).toEqual([
    { type: "bytes", bytes: 11, epoch: 0 },
    { type: "bytes", bytes: 13, epoch: 1 },
    { type: "broken", detail: "TypeError: network error" },
  ]);
});

test("a download's bodies end on the server's clock at the lane's latest end; a stop waits only for one ending soon", async () => {
  const bodies = [source(), source(), source(), source(), source(), source()];
  const asked: string[] = [];
  const bootLane = () =>
    boot<LaneCommand>("./lane.ts", {
      fetch: async (url: string, init: RequestInit) => {
        asked.push(url);
        const body = bodies[asked.length - 1]!;
        init.signal!.addEventListener("abort", () => body.controller().error(init.signal!.reason));
        return new Response(body.stream);
      },
    });
  const at = (ms: number) => performance.timeOrigin + performance.now() + ms;
  const download = (until: number | null): LaneCommand => ({
    type: "download",
    url: "https://peer.example/download?bytes=1",
    request: RECIPE,
    epoch: 0,
    streams: 1,
    timed: true,
    until: until === null ? null : at(until),
  });
  const lane = await bootLane();
  lane.send(download(5_000));
  await elapse(0);
  // The stage's end moved: the next body follows it.
  lane.send({ type: "until", until: at(6_400) });
  await elapse(5_000);
  bodies[0]!.controller().close();
  await elapse(1_000);
  lane.send({ type: "stop" });
  await elapse(0);
  expect(lane.types()).toEqual([]);
  bodies[1]!.controller().close();
  await elapse(0);
  expect([asked, lane.types()]).toEqual([
    [
      "https://peer.example/download?bytes=1&ms=5000",
      "https://peer.example/download?bytes=1&ms=1400",
    ],
    ["stopped"],
  ]);
  // A body far from its end, as an early finish leaves, is not waited for.
  const cut = await bootLane();
  cut.send(download(5_000));
  await elapse(0);
  cut.send({ type: "stop" });
  await elapse(0);
  expect(cut.types()).toEqual(["stopped"]);
  // One that ends early and short came from something else; before the stage's end is known, bodies are short.
  const early = await bootLane();
  early.send(download(null));
  await elapse(0);
  expect(asked.at(-1)).toBe("https://peer.example/download?bytes=1&ms=250");
  bodies[3]!.controller().close();
  await elapse(0);
  expect(early.posted).toEqual([
    {
      type: "ended",
      ending: { reason: "connection-lost", retry: true, detail: "the download ended early" },
    },
  ]);
  // An HTTP/1.1 body keeps its length, which no proxy re-chunks, and a stop cuts it.
  const sized = await bootLane();
  sized.send({ ...download(5_000), timed: false } as LaneCommand);
  await elapse(0);
  expect(asked.at(-1)).toBe("https://peer.example/download?bytes=1");
  sized.send({ type: "stop" });
  await elapse(0);
  expect(sized.types()).toEqual(["stopped"]);
  const waited = await bootLane();
  waited.send(download(300));
  await elapse(0);
  waited.send({ type: "stop" });
  await elapse(0);
  expect(waited.types()).toEqual([]);
  waited.send({ type: "stop" });
  await elapse(0);
  expect(waited.types()).toEqual(["stopped"]);
});

test("a flush reports the bytes counted so far before it answers", async () => {
  const body = source();
  const lane = await boot<LaneCommand>("./lane.ts", {
    fetch: async () => new Response(body.stream),
  });
  lane.send({
    type: "download",
    url: "https://peer.example/download?bytes=1",
    request: RECIPE,
    epoch: 1,
    streams: 1,
    timed: true,
    until: 0,
  });
  await elapse(0);
  body.push("counted");
  await elapse(0);
  const at = performance.timeOrigin + performance.now();
  lane.send({ type: "flush", at });
  lane.send({ type: "flush", at });
  const flushed = { type: "flushed", total: 7, at };
  expect(lane.posted).toEqual([{ type: "bytes", bytes: 7, epoch: 1 }, flushed, flushed]);
});

test("a stopped lane aborts its request, falls silent, acknowledges, and carries its next lane from a fresh count", async () => {
  const bodies = [source(), source()];
  let fetches = 0;
  const lane = await boot<LaneCommand>("./lane.ts", {
    fetch: async (_: string, init: RequestInit) => {
      const body = bodies[fetches++]!;
      init.signal!.addEventListener("abort", () => body.controller().error(init.signal!.reason));
      return new Response(body.stream);
    },
  });
  const download = {
    type: "download",
    url: "https://peer.example/download",
    request: RECIPE,
    epoch: 1,
    streams: 1,
    timed: false,
    until: null,
  } as const;
  lane.send(download);
  await elapse(0);
  bodies[0]!.push("first");
  lane.send({ type: "stop" });
  await elapse(0);
  expect(lane.types()).toEqual(["stopped"]);
  lane.send(download);
  await elapse(0);
  bodies[1]!.push("next");
  await elapse(0);
  const at = performance.timeOrigin + performance.now();
  lane.send({ type: "flush", at });
  expect(lane.posted.slice(1)).toEqual([
    { type: "bytes", bytes: 4, epoch: 1 },
    { type: "flushed", total: 4, at },
  ]);
});

test("a download whose answer ends short ends the lane, so its owner paces the reopen", async () => {
  let fetches = 0;
  const lane = await boot<LaneCommand>("./lane.ts", {
    fetch: async () => (fetches++, new Response("<html>portal</html>")),
  });
  lane.send({
    type: "download",
    url: "https://peer.example/download?bytes=1",
    request: RECIPE,
    epoch: 0,
    streams: 1,
    timed: true,
    until: 0,
  });
  await elapse(0);
  expect(fetches).toBe(1);
  expect(lane.posted.at(-1)).toMatchObject({
    type: "ended",
    ending: { reason: "connection-lost", retry: true },
  });
});

test("an upload drains each answer and breaks on one past 64 KiB rather than keeping it", async () => {
  const endless = source();
  const answers = [new Response("ok"), new Response(endless.stream)];
  const lane = await boot<LaneCommand>("./lane.ts", { fetch: async () => answers.shift() });
  lane.send({
    type: "upload",
    url: "https://peer.example/upload",
    request: RECIPE,
    streams: 1,
    sharing: 1,
    rttMs: null,
  });
  await elapse(0);
  for (let chunk = 0; chunk < 3; chunk++) endless.push("x".repeat(32 * 1024));
  await elapse(0);
  expect(lane.types()).toEqual(["posted", "broken"]);
  expect(lane.posted.at(-1)).toEqual({ type: "broken", detail: "the upload answer was too large" });
});

test("an upload lane rebuilds its file-backed part when its largest POST grows past eight parts", async () => {
  const parts: number[] = [];
  const lane = await boot<LaneCommand>("./lane.ts", {
    fetch: async () => new Response(null, { status: 500 }),
    Response: class extends Response {
      constructor(body?: BodyInit | null, init?: ResponseInit) {
        if (body instanceof Blob) parts.push(body.size);
        super(body, init);
      }
    },
  });
  for (const sharing of [4, 4, 1]) {
    lane.send({
      type: "upload",
      url: "https://peer.example/upload",
      request: RECIPE,
      streams: 1,
      sharing,
      rttMs: null,
    });
    await elapse(0);
  }
  // Firefox's 128 MiB budget: 32 MiB POSTs take 4 MiB parts, a lone lane's 128 MiB POSTs 16 MiB ones.
  expect(parts).toEqual([4 * 1024 * 1024, 16 * 1024 * 1024]);
});

test("upload POSTs grow past their turnaround, so one lane stays about 90 % busy on a long path", async () => {
  // At 100 Mbit/s a POST sends its body, then its answer arrives one round trip later.
  const BYTES_PER_MS = 12_500;
  for (const [pathMs, rttMs] of [
    [50, 50],
    [200, 200],
    [200, null],
  ] as const) {
    const sends: { at: number; ms: number }[] = [];
    let done = false;
    const lane = await boot<LaneCommand>("./lane.ts", {
      fetch: (_: string, init: RequestInit) =>
        new Promise((resolve) => {
          const ms = (init.body as Blob).size / BYTES_PER_MS;
          sends.push({ at: performance.now(), ms });
          setTimeout(() => done || resolve(new Response("ok")), ms + pathMs);
        }),
    });
    lane.send({
      type: "upload",
      url: "https://peer.example/upload",
      request: RECIPE,
      streams: 1,
      sharing: 1,
      rttMs,
    });
    await elapse(30_000);
    done = true;
    const steady = sends.slice(-4);
    const busy = steady.slice(0, -1).map(({ at, ms }, i) => ms / (steady[i + 1]!.at - at));
    for (const share of busy) expect(share, `${pathMs} ms, RTT ${rttMs}`).toBeGreaterThan(0.89);
  }
});

test("a session's stop finalizes its upload id, then acknowledges on the terminal record or after 1 s", async () => {
  const deletes: [string, RequestInit][] = [];
  const session = await boot<SessionCommand>("./session.ts", {
    WebTransport: FakeWebTransport,
    fetch: async (url: string, init: RequestInit) => (
      deletes.push([url, init]), new Response(null, { status: 204 })
    ),
  });
  const feed = "https://peer.example/upload/progress?id=u1";
  const upload: SessionCommand = {
    type: "upload",
    url: "https://peer.example/wt/upload?id=u1",
    datagrams: false,
    streams: 2,
    finalize: { url: feed, request: RECIPE },
  };
  for (const terminal of [true, false]) {
    session.posted.length = deletes.length = 0;
    session.send(upload);
    const transport = FakeWebTransport.dialed.at(-1)!;
    transport.accept();
    const records = source();
    transport.open(records.stream);
    records.push('{"type":"ready"}\n{"type":"progress","bytes":10,"nanos":5}\n');
    await elapse(0);
    session.send({ type: "stop" });
    await elapse(0);
    expect(deletes).toEqual([
      [feed, { ...RECIPE, method: "DELETE", cache: "no-store", signal: expect.any(AbortSignal) }],
    ]);
    expect(session.types()).toEqual(["handshake", "record", "record"]);
    if (terminal) records.push('{"type":"complete","bytes":12,"nanos":6}\n');
    else {
      await elapse(995);
      expect(session.types()).toEqual(["handshake", "record", "record"]);
      await elapse(5);
    }
    await elapse(0);
    expect(session.types().slice(3)).toEqual(terminal ? ["record", "stopped"] : ["stopped"]);
  }
});

test("a session relays a flood of receiver progress as its newest record every 50 ms", async () => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  session.send({
    type: "upload",
    url: "https://peer.example/wt/upload?id=u1",
    datagrams: false,
    streams: 1,
    finalize: { url: "https://peer.example/upload/progress?id=u1", request: RECIPE },
  });
  const transport = FakeWebTransport.dialed.at(-1)!;
  transport.accept();
  const records = source();
  transport.open(records.stream);
  records.push(
    Array.from(
      { length: 1_000 },
      (_, i) => `{"type":"progress","bytes":${i + 1},"nanos":${i + 1}}\n`,
    ).join(""),
  );
  await elapse(0);
  await elapse(50);
  expect(
    session.posted.filter(({ type }) => type === "record").map(({ record }) => record),
  ).toEqual([
    { type: "progress", bytes: 1, nanos: 1 },
    { type: "progress", bytes: 1_000, nanos: 1_000 },
  ]);
});

const datagrams = async (epoch: number) => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  session.send({
    type: "download",
    url: "https://peer.example/wt/download?datagrams=1",
    datagrams: true,
    epoch,
  });
  const transport = FakeWebTransport.dialed.at(-1)!;
  transport.accept();
  await elapse(0);
  return { session, transport };
};

test("a datagram download counts every byte", async () => {
  const { session, transport } = await datagrams(0);
  for (let i = 0; i < 1_600; i++) transport.datagram(1_200);
  // The fake clock stands still, so the loop reads every datagram without yielding.
  await task();
  session.send({ type: "flush", at: performance.timeOrigin + performance.now() });
  const counted = session.posted.flatMap((report) =>
    report.type === "bytes" ? [report.bytes as number] : [],
  );
  expect(counted.reduce((sum, bytes) => sum + bytes, 0)).toBe(1_600 * 1_200);
});

test("a final flush counts the datagrams read by its cutoff and none after, however late it is handled", async () => {
  const { session, transport } = await datagrams(1);
  await elapse(20);
  transport.datagram(1_200);
  await elapse(10);
  const at = performance.timeOrigin + performance.now();
  // A datagram is read after the cutoff, and the flush is handled 50 ms late.
  await elapse(10);
  transport.datagram(1_200);
  await elapse(40);
  session.send({ type: "flush", at });
  expect(session.posted.find(({ type }) => type === "flushed")).toEqual({
    type: "flushed",
    total: 1_200,
    at,
  });
});

test("a session reopened in its epoch keeps the epoch's count, and a stopped one falls silent for the next lane", async () => {
  const { session, transport } = await datagrams(3);
  transport.datagram(1_000);
  await task();
  transport.reset();
  await elapse(300);
  session.send({
    type: "download",
    url: "https://peer.example/wt/download?datagrams=1",
    datagrams: true,
    epoch: 3,
  });
  const reopened = FakeWebTransport.dialed.at(-1)!;
  reopened.accept();
  await elapse(0);
  reopened.datagram(500);
  await task();
  const flushed = () => session.posted.findLast(({ type }) => type === "flushed");
  session.send({ type: "flush", at: performance.timeOrigin + performance.now() });
  expect(flushed()).toMatchObject({ total: 1_500 });
  session.send({ type: "stop" });
  await elapse(300);
  expect(session.types().at(-1)).toBe("stopped");
  const before = session.posted.length;
  reopened.datagram(700);
  await task();
  expect(session.posted.length).toBe(before);
  session.send({ type: "flush", at: performance.timeOrigin + performance.now() });
  expect(flushed()).toMatchObject({ total: 0 });
});

test("a slow datagram download reports each datagram as it arrives", async () => {
  // It gets its own epoch, since an earlier test's loop may still post into the shared port.
  const { session, transport } = await datagrams(7);
  const reported = () =>
    session.posted.filter((report) => report.type === "bytes" && report.epoch === 7).length;
  // Five datagrams a second arrive, each well past the report window.
  await elapse(200);
  transport.datagram(1_200);
  await elapse(0);
  expect(reported()).toBe(1);
  // The loop yields a task between datagrams, first a message and then a timer.
  await task();
  await elapse(180);
  transport.datagram(1_200);
  await elapse(0);
  expect(reported()).toBe(2);
});

test("a session whose streams broke waits for its close: a revocation asks for sign-in, silence is a break", async () => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  const download: SessionCommand = {
    type: "download",
    url: "https://peer.example/wt/download?streams=1",
    datagrams: false,
    epoch: 0,
  };
  session.send(download);
  FakeWebTransport.dialed.at(-1)!.accept();
  await elapse(0);
  // The server resets the streams just before its close arrives.
  FakeWebTransport.dialed.at(-1)!.reset();
  await elapse(20);
  FakeWebTransport.dialed.at(-1)!.end(REVOKED);
  await elapse(0);
  session.send(download);
  FakeWebTransport.dialed.at(-1)!.accept();
  await elapse(0);
  FakeWebTransport.dialed.at(-1)!.reset();
  await elapse(245);
  expect(session.posted).toHaveLength(3);
  await elapse(5);
  expect(session.posted).toEqual([
    { type: "handshake", opened: true },
    {
      type: "ended",
      ending: { reason: "sign-in-required", retry: false, detail: "webtransport session revoked" },
    },
    { type: "handshake", opened: true },
    { type: "broken", detail: "Error: stream reset" },
  ]);
});

test("a session's refused dial and one that lapsed unanswered both report a failed handshake, and redial its lease", async () => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  const download: SessionCommand = {
    type: "download",
    url: "https://peer.example/wt/download?streams=1&lease=gml_t",
    datagrams: false,
    epoch: 0,
  };
  session.send(download);
  await elapse(0);
  FakeWebTransport.dialed.at(-1)!.refuse();
  await elapse(0);
  session.send(download);
  await elapse(3_000);
  session.send(download);
  await elapse(0);
  expect(FakeWebTransport.dialed.map(({ url }) => new URL(url).searchParams.get("lease"))).toEqual([
    "gml_t",
    "gml_t",
    "gml_t",
  ]);
  expect(session.posted).toEqual([
    { type: "handshake", opened: false },
    { type: "broken", detail: "webtransport session closed" },
    { type: "handshake", opened: false },
    {
      type: "ended",
      ending: {
        reason: "path-failed",
        retry: true,
        detail: "webtransport session did not establish",
      },
    },
  ]);
});

/** The test answers for the server on this WebSocket. */
class FakeWebSocket {
  static opened: FakeWebSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;

  closes = 0;

  constructor(readonly url: string) {
    FakeWebSocket.opened.push(this);
  }

  send(text: string): void {
    queueMicrotask(() => this.onmessage?.({ data: `PONG,${text.slice("PING,".length)},0` }));
  }

  close(): void {
    this.closes++;
  }
}

const bus = (transport: "websocket" | "webtransport"): ProbeCommand => ({
  type: "start",
  url: `${transport === "websocket" ? "wss://peer.example/ws/ping" : "https://peer.example/wt/ping"}?lease=gml_t`,
  transport,
  intervalMs: 80,
  maxInFlight: 2,
});

test("a WebSocket dial past its budget stays, reported as an outage until it answers; a refused one waits for the owner", async () => {
  const probe = await boot<ProbeCommand>("./probe.ts", { WebSocket: FakeWebSocket });
  probe.send(bus("websocket"));
  // The browser queues the handshake, so closing it would only delay the next one.
  await elapse(3_500);
  expect(probe.types()).toEqual(["stalled"]);
  expect(FakeWebSocket.opened.map(({ closes }) => closes)).toEqual([0]);
  FakeWebSocket.opened.at(-1)!.onopen!();
  await elapse(0);
  expect(probe.types()).toEqual(["stalled", "ready", "resumed"]);
  // Only the owner redials a refused upgrade.
  FakeWebSocket.opened.at(-1)!.onclose!({ code: 1006, reason: "" });
  await elapse(5_000);
  expect(FakeWebSocket.opened).toHaveLength(1);
  probe.send({ type: "dial" });
  await elapse(0);
  FakeWebSocket.opened.at(-1)!.onclose!({ code: 1008, reason: "authentication required" });
  expect(FakeWebSocket.opened.map(({ url }) => new URL(url).searchParams.get("lease"))).toEqual([
    "gml_t",
    "gml_t",
  ]);
  expect(probe.types().slice(3)).toEqual(["stalled", "sign-in"]);
});

test("an owner's dial during a lapsed WebSocket dial follows that dial's end", async () => {
  const probe = await boot<ProbeCommand>("./probe.ts", { WebSocket: FakeWebSocket });
  probe.send(bus("websocket"));
  await elapse(3_500);
  expect(probe.types()).toEqual(["stalled"]);
  probe.send({ type: "dial" });
  await elapse(0);
  expect(FakeWebSocket.opened).toHaveLength(1);
  FakeWebSocket.opened.at(-1)!.onclose!({ code: 1006, reason: "" });
  await elapse(0);
  expect(FakeWebSocket.opened).toHaveLength(2);
});

test("a bus at the server's lifetime bound, or renewed, dials again on its own without an outage, unless it ended at once", async () => {
  const probe = await boot<ProbeCommand>("./probe.ts", { WebSocket: FakeWebSocket });
  probe.send(bus("websocket"));
  const first = FakeWebSocket.opened.at(-1)!;
  first.onopen!();
  await elapse(3_000);
  first.onclose!({ code: 4002, reason: "lifetime" });
  await elapse(0);
  const second = FakeWebSocket.opened.at(-1)!;
  expect(second).not.toBe(first);
  second.onopen!();
  await elapse(100);
  probe.send({ type: "renew" });
  await elapse(0);
  expect([second.closes, FakeWebSocket.opened.length]).toEqual([1, 3]);
  expect(probe.types().filter((type) => type === "stalled" || type === "renewing")).toEqual([
    "renewing",
    "renewing",
  ]);
  const third = FakeWebSocket.opened.at(-1)!;
  third.onopen!();
  third.onclose!({ code: 4002, reason: "lifetime" });
  await elapse(0);
  expect(probe.types().at(-1)).toBe("stalled");
  expect(FakeWebSocket.opened).toHaveLength(3);
});

test("a WebTransport bus whose datagrams failed waits for its close: a revocation asks for sign-in", async () => {
  const probe = await boot<ProbeCommand>("./probe.ts", { WebTransport: FakeWebTransport });
  probe.send(bus("webtransport"));
  await elapse(0);
  const transport = FakeWebTransport.dialed.at(-1)!;
  transport.accept();
  await elapse(0);
  transport.reset();
  await elapse(20);
  transport.end(REVOKED);
  await elapse(0);
  expect(probe.types()).toEqual(["handshake", "sign-in"]);
});
