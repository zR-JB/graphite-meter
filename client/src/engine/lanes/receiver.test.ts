import { afterEach, expect, jest, test } from "bun:test";
import { SignInRequired, type Checkpoint, type Ending, type ServerAuth } from "../types";
import { elapse } from "./fakes.testutil";
import { Receiver, type FeedRoute } from "./receiver";

type Script = (socket: FakeSocket) => void | Promise<void>;
class FakeSocket {
  static scripts: Script[] = [];
  static dialed: FakeSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;
  closed = false;

  constructor(readonly url: string) {
    FakeSocket.dialed.push(this);
    const script = FakeSocket.scripts.shift();
    queueMicrotask(() => void script?.(this));
  }

  open(): void {
    this.onopen?.();
  }

  send(...lines: string[]): void {
    for (const data of lines) this.onmessage?.({ data });
  }

  end(code = 1006, reason = ""): void {
    this.onclose?.({ code, reason });
  }

  close(): void {
    this.closed = true;
  }
}

const RealWebSocket = globalThis.WebSocket;
afterEach(() => Object.assign(globalThis, { WebSocket: RealWebSocket }));

const encoder = new TextEncoder();
const ndjson = (...chunks: string[]) =>
  new Response(
    new ReadableStream({
      start(controller) {
        for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
        controller.close();
      },
    }),
  );

/** A receiver whose feed socket plays `scripts`, whose HTTP feed answers from `answers`, and whose checkpoints answer `counters`. */
function follow(
  scripts: Script[],
  {
    counters = [] as { bytes: number; nanos: number }[],
    route = "websocket" as FeedRoute,
    ticket = (() => null) as ServerAuth["ticket"],
    answers = [] as ((init: RequestInit) => Response)[],
  } = {},
) {
  FakeSocket.scripts = scripts;
  FakeSocket.dialed = [];
  Object.assign(globalThis, { WebSocket: FakeSocket });
  const observed: [number, number, boolean][] = [];
  const endings: [Ending, boolean, number][] = [];
  const deletes: RequestInit[] = [];
  let signedIn = false;
  const auth: ServerAuth = {
    kind: "public",
    left: () => Infinity,
    request: () => ({ headers: {}, credentials: "omit", redirect: "error" }),
    fetch: async (url, init = {}) => {
      if (init.method === "DELETE") return deletes.push(init), new Response(null, { status: 204 });
      if (!url.includes("/upload/progress")) return Response.json(counters.shift());
      expect(url).toBe("https://peer.example/upload/progress?id=u1");
      return answers.shift()!(init);
    },
    ticket,
    ended: async () => false,
  };
  const receiver = new Receiver(
    {
      now: () => 0,
      observed: ({ bytes, nanos }: Checkpoint, grew) => observed.push([bytes, nanos, grew]),
      // The stage reopens what may retry. The test skips the pause.
      ended: async (ending, broken, delayMs) => (
        endings.push([ending, broken, delayMs]), ending.retry
      ),
      signIn: () => (signedIn = true),
    },
    auth,
    "https://peer.example",
    "u1",
    route,
  );
  return { receiver, observed, endings, deletes, signedIn: () => signedIn };
}

const settled = async (done: () => boolean) => {
  for (let turns = 0; !done() && turns < 100; turns++)
    await new Promise((resolve) => setTimeout(resolve, 0));
};

test("only advancing receiver observations pass, across a replacement feed socket and checkpoints", async () => {
  const feed = follow(
    [
      (socket) => {
        socket.open();
        socket.send(
          '{"type":"ready"}',
          '{"type":"progress","bytes":100,"nanos":1000}',
          '{"type":"progress","bytes":90,"nanos":3000}',
          "",
          '{"type":"progress","bytes":300,"nanos":2000}',
          '{"type":"progress","bytes":"7","nanos":9}',
          '{"truncated',
        );
        socket.end();
      },
      (socket) => {
        socket.open();
        socket.send(
          '{"type":"ready"}',
          '{"type":"progress","bytes":300,"nanos":1500}',
          '{"type":"progress","bytes":400,"nanos":4000}',
          '{"type":"complete","bytes":400,"nanos":4100}',
        );
      },
    ],
    {
      counters: [
        { bytes: 350, nanos: 3_000 },
        { bytes: 500, nanos: 5_000 },
      ],
    },
  );
  await settled(() => feed.observed.length === 4);
  await feed.receiver.opened(AbortSignal.timeout(100));
  // Bytes and receiver time only ever count together, so a regression in either is a stale observation.
  expect(feed.observed).toEqual([
    [100, 1_000, true],
    [300, 2_000, true],
    [400, 4_000, true],
    [400, 4_100, false],
  ]);
  expect(feed.endings).toEqual([
    [{ reason: "connection-lost", retry: true, detail: "progress socket closed" }, true, 300],
  ]);
  // A feed record that overtook the answer was taken after the request reached the receiver, so it answers instead.
  expect(await feed.receiver.checkpoint(AbortSignal.timeout(100))).toEqual({
    id: "u1",
    bytes: 400,
    nanos: 4_100,
    requestedAtMs: 0,
    receivedAtMs: 0,
  });
  expect(await feed.receiver.checkpoint(AbortSignal.timeout(100))).toMatchObject({
    bytes: 500,
    nanos: 5_000,
  });
  expect(feed.observed.at(-1)).toEqual([500, 5_000, true]);
  feed.receiver.close();
});

test("feed refusals count as busy until the feed opens, an unknown id rotates, and a revoked socket asks for sign-in", async () => {
  const refused = (code: string, message: string) => (socket: FakeSocket) => {
    socket.open();
    socket.send(`{"type":"error","code":"${code}","message":"${message}"}`);
  };
  const feed = follow([
    (socket) => socket.end(),
    refused("globalFull", "server full"),
    refused("clientFull", "client full"),
    (socket) => {
      socket.open();
      socket.send('{"type":"ready"}');
      refused("globalFull", "server full")(socket);
    },
    refused("invalid", "unknown upload id"),
  ]);
  await settled(() => feed.endings.length === 5);
  expect(
    feed.endings.map(([{ reason, rotate, detail }, broken, delayMs]) => [
      reason,
      rotate,
      detail,
      broken,
      delayMs,
    ]),
  ).toEqual([
    ["connection-lost", undefined, "progress socket closed", true, 300],
    ["server-busy", undefined, "server full", false, 300],
    ["server-busy", undefined, "client full", false, 300],
    // The run decides the one replacement.
    ["server-busy", undefined, "server full", false, 300],
    ["protocol-error", true, "unknown upload id", false, 300],
  ]);
  expect(feed.receiver.busy).toBe(false);
  expect(FakeSocket.dialed.map(({ url }) => url)).toEqual(
    Array(5).fill("wss://peer.example/ws/progress?id=u1"),
  );
  for (const ticket of [
    () => {
      throw new SignInRequired({ id: "peer", url: "https://peer.example", name: "Peer" });
    },
    () => null,
  ]) {
    const ended = follow([(socket) => socket.end(1008, "authentication required")], { ticket });
    await settled(ended.signedIn);
    expect(ended.endings).toEqual([]);
  }
  const broken = follow([], {
    ticket: () => {
      throw new Error("not this server's origin");
    },
  });
  await settled(() => broken.endings.length > 0);
  expect(broken.endings).toEqual([
    [
      { reason: "protocol-error", retry: false, detail: "Error: not this server's origin" },
      false,
      300,
    ],
  ]);
});

test("a feed socket silent for 1.5 s reconnects, while heartbeats keep a quiet one open", async () => {
  jest.useFakeTimers();
  try {
    const feed = follow([(socket) => socket.open(), () => {}]);
    await elapse(0);
    const [first] = FakeSocket.dialed;
    first!.send('{"type":"ready"}');
    for (let second = 0; second < 4; second++) {
      await elapse(1_000);
      first!.send("");
    }
    await elapse(1_200);
    expect(feed.endings).toEqual([]);
    await elapse(300);
    expect(feed.endings).toEqual([
      [
        { reason: "connection-lost", retry: true, detail: "progress socket went silent" },
        false,
        300,
      ],
    ]);
    expect(first!.closed).toBe(true);
    expect(FakeSocket.dialed).toHaveLength(2);
    feed.receiver.close();
  } finally {
    jest.useRealTimers();
  }
});

test("an HTTP feed reads split records until its terminal one, backs off refusals as busy, reopens a closed or broken feed, and asks for sign-in", async () => {
  const feed = follow([], {
    route: "http",
    answers: [
      () => {
        throw new TypeError("Failed to fetch");
      },
      () =>
        new Response("refused", {
          status: 503,
          headers: { "X-Graphite-Upload-Refusal": "globalFull", "Retry-After": "1" },
        }),
      ({ headers }) => {
        expect(headers).toEqual({ accept: "application/x-ndjson" });
        return ndjson('{"type":"ready"}\n{"type":"progress","bytes":100,"na', 'nos":1000}\n');
      },
      () =>
        ndjson(
          '{"type":"progress","bytes":200,"nanos":2000}\n{"type":"complete","bytes":200,"nanos":2100}\n',
        ),
    ],
  });
  await settled(() => feed.observed.length === 3);
  expect(feed.observed).toEqual([
    [100, 1_000, true],
    [200, 2_000, true],
    [200, 2_100, false],
  ]);
  expect(
    feed.endings.map(([{ reason, rotate, detail }, broken, delayMs]) => [
      reason,
      rotate,
      detail,
      broken,
      delayMs,
    ]),
  ).toEqual([
    ["connection-lost", undefined, "TypeError: Failed to fetch", true, 300],
    ["server-busy", undefined, "progress returned HTTP 503", false, 1_000],
    ["connection-lost", undefined, "progress stream closed", false, 300],
  ]);
  expect(FakeSocket.dialed).toEqual([]);
  const ended = follow([], {
    route: "http",
    answers: [
      () => {
        throw new SignInRequired({ id: "peer", url: "https://peer.example", name: "Peer" });
      },
    ],
  });
  await settled(ended.signedIn);
  expect(ended.endings).toEqual([]);
});

test("an HTTP feed silent for 1.5 s reconnects, while heartbeats keep a quiet one open", async () => {
  jest.useFakeTimers();
  try {
    const streams: ReadableStreamDefaultController<Uint8Array>[] = [];
    // The answer's body errors when its request is aborted, as a browser's does.
    const open = ({ signal }: RequestInit) =>
      new Response(
        new ReadableStream({
          start(controller) {
            streams.push(controller);
            signal!.addEventListener("abort", () => controller.error(signal!.reason));
          },
        }),
      );
    const feed = follow([], { route: "http", answers: [open, open] });
    await elapse(0);
    streams[0]!.enqueue(encoder.encode('{"type":"ready"}\n'));
    for (let second = 0; second < 4; second++) {
      await elapse(1_000);
      streams[0]!.enqueue(encoder.encode("\n"));
    }
    await elapse(1_200);
    expect(feed.endings).toEqual([]);
    await elapse(300);
    expect(feed.endings).toEqual([
      [
        { reason: "connection-lost", retry: true, detail: "progress stream went silent" },
        false,
        300,
      ],
    ]);
    expect(streams).toHaveLength(2);
    feed.receiver.close();
  } finally {
    jest.useRealTimers();
  }
});

test("a session's id is backed up by a DELETE at finish, unless a terminal record that passed the guard arrived", async () => {
  const progress = { type: "progress", bytes: 100, nanos: 1_000 } as const;
  const stale = follow([], { route: "session" });
  stale.receiver.record(progress);
  // A regressing terminal record is no evidence that the id finalized.
  stale.receiver.record({ type: "complete", bytes: 50, nanos: 500 });
  await stale.receiver.finish();
  expect(stale.deletes).toEqual([{ method: "DELETE", cache: "no-store", keepalive: true }]);
  const complete = follow([], { route: "session" });
  complete.receiver.record(progress);
  complete.receiver.record({ ...progress, type: "complete" });
  await complete.receiver.finish();
  expect(complete.deletes).toEqual([]);
});
