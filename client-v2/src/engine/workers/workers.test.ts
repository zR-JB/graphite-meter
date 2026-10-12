import { afterEach, beforeEach, expect, jest, spyOn, test } from "bun:test";
import type { LaneCommand, ProbeCommand, SessionCommand } from "../../protocol/messages";
import { elapse } from "../lanes/fakes.testutil";

const encoder = new TextEncoder();
const GLOBALS = ["onmessage", "postMessage", "fetch", "WebTransport", "WebSocket"] as const;
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

/** Evaluates a fresh copy of a worker module against `globals`: what it posts, and its owner's side of the port. */
async function boot<Command>(module: string, globals: Partial<Record<(typeof GLOBALS)[number], unknown>>) {
  const posted: { type: string; [field: string]: unknown }[] = [];
  Object.assign(globalThis, { onmessage: null, postMessage: (report: (typeof posted)[number]) => posted.push(report) });
  Object.assign(globalThis, globals);
  await import(`${module}?realm=${realms++}`);
  const port = globalThis.onmessage as unknown as (event: { data: Command }) => void;
  return { posted, send: (data: Command) => port({ data }), types: () => posted.map(({ type }) => type) };
}

/** A controllable stream of byte chunks or NDJSON lines. */
function source() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({ start: (c) => void (controller = c) });
  return { stream, push: (text: string) => controller.enqueue(encoder.encode(text)), controller: () => controller };
}

/** A WebTransport session the test answers for the server: handshake, server streams, datagrams and its close. */
class FakeWebTransport {
  static dialed: FakeWebTransport[] = [];
  readonly ready: Promise<void>;
  readonly closed: Promise<WebTransportCloseInfo>;
  readonly accept: () => void;
  readonly refuse: () => void;
  /** Closes the session; the first close, ours or the server's, is the one `closed` reports. */
  readonly end: (info: WebTransportCloseInfo) => void;
  #streams!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;
  #datagrams!: ReadableStreamDefaultController<Uint8Array>;
  readonly incomingUnidirectionalStreams = new ReadableStream<ReadableStream<Uint8Array>>({
    start: (controller) => void (this.#streams = controller),
  });
  readonly datagrams = {
    maxDatagramSize: 1_200,
    readable: new ReadableStream<Uint8Array>({ start: (controller) => void (this.#datagrams = controller) }),
    writable: new WritableStream<Uint8Array>(),
  };

  constructor(readonly url: string) {
    const ready = Promise.withResolvers<void>();
    const closed = Promise.withResolvers<WebTransportCloseInfo>();
    [this.ready, this.closed, this.accept, this.end] = [ready.promise, closed.promise, ready.resolve, closed.resolve];
    this.refuse = () => {
      ready.reject(new Error("Opening handshake failed."));
      closed.reject(new Error("Opening handshake failed."));
    };
    FakeWebTransport.dialed.push(this);
  }

  /** The server opens a stream. */
  open(stream: ReadableStream<Uint8Array>): void {
    this.#streams.enqueue(stream);
  }

  /** The server sends a datagram. */
  datagram(bytes: number): void {
    this.#datagrams.enqueue(new Uint8Array(bytes));
  }

  /** The server resets the session's streams and datagrams. */
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

const RECIPE = { headers: { authorization: "Bearer g" }, credentials: "omit", redirect: "error" } as const;
const REVOKED = { closeCode: 3, reason: "authentication required" };

test("a download counts bytes into the epoch they were read in: a measure drops what was not yet reported", async () => {
  const body = source();
  const lane = await boot<LaneCommand>("./lane.ts", { fetch: async () => new Response(body.stream) });
  lane.send({ type: "download", url: "https://peer.example/download", request: RECIPE, epoch: 0, streams: 1 });
  await elapse(0);
  body.push("warmup");
  await elapse(0);
  lane.send({ type: "measure", epoch: 1 });
  await elapse(50);
  body.push("window");
  await elapse(0);
  body.controller().error(new TypeError("network error"));
  await elapse(0);
  expect(lane.posted).toEqual([
    { type: "bytes", bytes: 6, epoch: 1 },
    { type: "broken", detail: "TypeError: network error" },
  ]);
});

test("a flush reports the bytes counted so far before it answers", async () => {
  const body = source();
  const lane = await boot<LaneCommand>("./lane.ts", { fetch: async () => new Response(body.stream) });
  lane.send({ type: "download", url: "https://peer.example/download", request: RECIPE, epoch: 1, streams: 1 });
  await elapse(0);
  body.push("counted");
  await elapse(0);
  lane.send({ type: "flush" });
  lane.send({ type: "flush" });
  expect(lane.posted).toEqual([{ type: "bytes", bytes: 7, epoch: 1 }, { type: "flushed" }, { type: "flushed" }]);
});

test("a download whose answer ends short ends the lane, so its owner paces the reopen", async () => {
  let fetches = 0;
  const lane = await boot<LaneCommand>("./lane.ts", {
    fetch: async () => (fetches++, new Response("<html>portal</html>")),
  });
  lane.send({ type: "download", url: "https://peer.example/download", request: RECIPE, epoch: 0, streams: 1 });
  await elapse(0);
  expect(fetches).toBe(1);
  expect(lane.posted.at(-1)).toMatchObject({ type: "ended", ending: { reason: "connection-lost", retry: true } });
});

test("an upload drains each answer and breaks on one past 64 KiB rather than keeping it", async () => {
  const endless = source();
  const answers = [new Response("ok"), new Response(endless.stream)];
  const lane = await boot<LaneCommand>("./lane.ts", { fetch: async () => answers.shift() });
  lane.send({ type: "upload", url: "https://peer.example/upload", request: RECIPE, streams: 1, rttMs: null });
  await elapse(0);
  for (let chunk = 0; chunk < 3; chunk++) endless.push("x".repeat(32 * 1024));
  await elapse(0);
  expect(lane.types()).toEqual(["posted", "broken"]);
  expect(lane.posted.at(-1)).toEqual({ type: "broken", detail: "the upload answer was too large" });
});

test("upload POSTs grow past their turnaround, so one lane stays about 90 % busy on a long path", async () => {
  // 100 Mbit/s: a POST sends its body, then its answer arrives one round trip later.
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
    lane.send({ type: "upload", url: "https://peer.example/upload", request: RECIPE, streams: 1, rttMs });
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
    fetch: async (url: string, init: RequestInit) => (deletes.push([url, init]), new Response(null, { status: 204 })),
  });
  const feed = "https://peer.example/upload/progress?id=u1";
  const upload: SessionCommand = {
    type: "upload",
    url: "https://peer.example/wt/upload?id=u1",
    ticket: null,
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
    expect(session.types()).toEqual(["record", "record"]);
    if (terminal) records.push('{"type":"complete","bytes":12,"nanos":6}\n');
    else {
      await elapse(995);
      expect(session.types()).toEqual(["record", "record"]);
      await elapse(5);
    }
    await elapse(0);
    expect(session.types().slice(2)).toEqual(terminal ? ["record", "stopped"] : ["stopped"]);
  }
});

test("a session relays a flood of receiver progress as its newest record every 50 ms", async () => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  session.send({
    type: "upload",
    url: "https://peer.example/wt/upload?id=u1",
    ticket: null,
    datagrams: false,
    streams: 1,
    finalize: { url: "https://peer.example/upload/progress?id=u1", request: RECIPE },
  });
  const transport = FakeWebTransport.dialed.at(-1)!;
  transport.accept();
  const records = source();
  transport.open(records.stream);
  records.push(
    Array.from({ length: 1_000 }, (_, i) => `{"type":"progress","bytes":${i + 1},"nanos":${i + 1}}\n`).join(""),
  );
  await elapse(0);
  await elapse(50);
  expect(session.posted.filter(({ type }) => type === "record").map(({ record }) => record)).toEqual([
    { type: "progress", bytes: 1, nanos: 1 },
    { type: "progress", bytes: 1_000, nanos: 1_000 },
  ]);
});

test("a datagram download reads the clock once per 16 datagrams and still counts every byte", async () => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  session.send({
    type: "download",
    url: "https://peer.example/wt/download?datagrams=1",
    ticket: null,
    datagrams: true,
    epoch: 0,
  });
  const transport = FakeWebTransport.dialed.at(-1)!;
  transport.accept();
  await elapse(0);
  const clock = spyOn(performance, "now");
  for (let i = 0; i < 1_600; i++) transport.datagram(1_200);
  for (let turn = 0; turn < 10_000; turn++) await null;
  expect(clock.mock.calls.length).toBeLessThanOrEqual(1_600 / 16 + 1);
  clock.mockRestore();
  session.send({ type: "flush" });
  const counted = session.posted.flatMap((report) => (report.type === "bytes" ? [report.bytes as number] : []));
  expect(counted.reduce((sum, bytes) => sum + bytes, 0)).toBe(1_600 * 1_200);
});

test("a slow datagram download reports what arrived within 100 ms, not after 16 datagrams", async () => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  session.send({
    type: "download",
    url: "https://peer.example/wt/download?datagrams=1",
    ticket: null,
    datagrams: true,
    epoch: 0,
  });
  const transport = FakeWebTransport.dialed.at(-1)!;
  transport.accept();
  await elapse(0);
  // Five datagrams a second: sixteen would take over three, past the run's 1.5 s silence rule.
  const reported = () => session.posted.filter(({ type }) => type === "bytes").length;
  transport.datagram(1_200);
  await elapse(0);
  expect(reported()).toBe(0);
  await elapse(100);
  expect(reported()).toBe(1);
  await elapse(100);
  transport.datagram(1_200);
  await elapse(100);
  expect(reported()).toBe(2);
});

test("a session whose streams broke waits for its close: a revocation asks for sign-in, silence is a break", async () => {
  const session = await boot<SessionCommand>("./session.ts", { WebTransport: FakeWebTransport });
  const download: SessionCommand = {
    type: "download",
    url: "https://peer.example/wt/download?streams=1",
    ticket: null,
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
  expect(session.posted).toHaveLength(1);
  await elapse(5);
  expect(session.posted).toEqual([
    { type: "ended", ending: { reason: "sign-in-required", retry: false, detail: "webtransport session revoked" } },
    { type: "broken", detail: "Error: stream reset" },
  ]);
});

test("a session's refused dial spends its ticket, and one that lapsed unanswered reuses it", async () => {
  let minted = 0;
  const session = await boot<SessionCommand>("./session.ts", {
    WebTransport: FakeWebTransport,
    fetch: async () => Response.json({ token: `t${++minted}`, expires: Date.now() + 30_000 }),
  });
  const download: SessionCommand = {
    type: "download",
    url: "https://peer.example/wt/download?streams=1",
    ticket: { url: "https://peer.example/wt/session?target=wt", request: RECIPE },
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
  expect(FakeWebTransport.dialed.map(({ url }) => new URL(url).searchParams.get("token"))).toEqual(["t1", "t2", "t2"]);
  expect(session.posted).toEqual([
    { type: "broken", detail: "webtransport session closed" },
    { type: "ended", ending: { reason: "timeout", retry: true, detail: "webtransport session did not establish" } },
  ]);
});

/** A WebSocket the test answers for the server. */
class FakeWebSocket {
  static opened: FakeWebSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;

  closes = 0;

  constructor(readonly url: string) {
    FakeWebSocket.opened.push(this);
  }

  /** The server answers the probe it was sent. */
  send(text: string): void {
    queueMicrotask(() => this.onmessage?.({ data: `PONG,${text.slice("PING,".length)},0` }));
  }

  close(): void {
    this.closes++;
  }
}

const bus = (transport: "websocket" | "webtransport", ticket: boolean): ProbeCommand => ({
  type: "start",
  url: transport === "websocket" ? "wss://peer.example/ws/ping" : "https://peer.example/wt/ping",
  transport,
  ticket: ticket ? { url: "https://peer.example/ws/session?target=wss", request: RECIPE } : null,
  intervalMs: 80,
  maxInFlight: 2,
});

test("a WebSocket dial past its budget stays, reported as an outage until it answers; a refused one spends its ticket", async () => {
  let minted = 0;
  const probe = await boot<ProbeCommand>("./probe.ts", {
    WebSocket: FakeWebSocket,
    fetch: async () => Response.json({ token: `t${++minted}`, expires: Date.now() + 30_000 }),
  });
  probe.send(bus("websocket", true));
  // The browser queues the handshake: closing it would only delay the next one.
  await elapse(3_500);
  expect(probe.types()).toEqual(["stalled"]);
  expect(FakeWebSocket.opened.map(({ closes }) => closes)).toEqual([0]);
  FakeWebSocket.opened.at(-1)!.onopen!();
  await elapse(0);
  expect(probe.types()).toEqual(["stalled", "ready", "resumed"]);
  // A refused upgrade presented its ticket: the server spent it whatever it answered.
  FakeWebSocket.opened.at(-1)!.onclose!({ code: 1006, reason: "" });
  await elapse(100);
  FakeWebSocket.opened.at(-1)!.onclose!({ code: 1008, reason: "authentication required" });
  expect(FakeWebSocket.opened.map(({ url }) => new URL(url).searchParams.get("token"))).toEqual(["t1", "t2"]);
  expect(probe.types().slice(3)).toEqual(["stalled", "sign-in"]);
});

test("a WebTransport bus whose datagrams failed waits for its close: a revocation asks for sign-in", async () => {
  const probe = await boot<ProbeCommand>("./probe.ts", { WebTransport: FakeWebTransport });
  probe.send(bus("webtransport", false));
  await elapse(0);
  const transport = FakeWebTransport.dialed.at(-1)!;
  transport.accept();
  await elapse(0);
  transport.reset();
  await elapse(20);
  transport.end(REVOKED);
  await elapse(0);
  expect(probe.types()).toEqual(["sign-in"]);
});
