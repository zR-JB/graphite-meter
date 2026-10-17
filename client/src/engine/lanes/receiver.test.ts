import { expect, jest, test } from "bun:test";
import { SignInRequired, type Checkpoint, type Ending, type ServerAuth } from "../types";
import { elapse } from "./fakes.testutil";
import { Receiver } from "./receiver";

const encoder = new TextEncoder();

/** An NDJSON body arriving in these chunks. */
const ndjson = (...chunks: string[]) =>
  new Response(
    new ReadableStream({
      start(controller) {
        for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
        controller.close();
      },
    }),
  );

/** A receiver following an HTTP feed whose answers come from `script`, one per request, and whose checkpoints answer
    `counters` in turn; `carried` leaves the feed to a session. */
function follow(
  script: ((init: RequestInit) => Response)[],
  counters: { bytes: number; nanos: number }[] = [],
  carried = false,
) {
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
      return url.includes("/checkpoint") ? Response.json(counters.shift()) : script.shift()!(init);
    },
    ticket: () => null,
    ended: async () => false,
  };
  const receiver = new Receiver(
    {
      now: () => 0,
      observed: ({ bytes, nanos }: Checkpoint, grew) => observed.push([bytes, nanos, grew]),
      // The stage reopens what may retry; the test skips the pause.
      ended: async (ending, broken, delayMs) => (endings.push([ending, broken, delayMs]), ending.retry),
      signIn: () => (signedIn = true),
    },
    auth,
    "https://peer.example",
    "u1",
    carried,
  );
  return { receiver, observed, endings, deletes, signedIn: () => signedIn };
}

const settled = async (done: () => boolean) => {
  for (let turns = 0; !done() && turns < 100; turns++) await new Promise((resolve) => setTimeout(resolve, 0));
};

test("only advancing receiver observations pass, across split records, a replacement feed and checkpoints", async () => {
  const feed = follow(
    [
      () =>
        ndjson(
          '{"type":"ready"}\n{"type":"progress","bytes":100,"na',
          'nos":1000}\n{"type":"progress","bytes":90,"nanos":3000}\n\n{"type":"progress",',
          '"bytes":300,"nanos":2000}\n{"type":"progress","bytes":"7","nanos":9}\n{"truncated',
        ),
      () =>
        ndjson(
          '{"type":"ready"}\n{"type":"progress","bytes":300,"nanos":1500}\n',
          '{"type":"progress","bytes":400,"nanos":4000}\n{"type":"complete","bytes":400,"nanos":4100}\n',
        ),
    ],
    [
      { bytes: 350, nanos: 3_000 },
      { bytes: 500, nanos: 5_000 },
    ],
  );
  await settled(() => feed.observed.length === 4);
  await feed.receiver.opened(AbortSignal.timeout(100));
  // Bytes and receiver time only ever count together: a regression in either is a stale observation.
  expect(feed.observed).toEqual([
    [100, 1_000, true],
    [300, 2_000, true],
    [400, 4_000, true],
    [400, 4_100, false],
  ]);
  expect(feed.endings).toEqual([
    [{ reason: "connection-lost", retry: true, detail: "progress stream closed" }, false, 300],
  ]);
  // A feed record that overtook the answer was taken after the request reached the receiver, so it answers instead.
  expect(await feed.receiver.checkpoint(AbortSignal.timeout(100))).toEqual({
    id: "u1",
    bytes: 400,
    nanos: 4_100,
    requestedAtMs: 0,
    receivedAtMs: 0,
  });
  expect(await feed.receiver.checkpoint(AbortSignal.timeout(100))).toMatchObject({ bytes: 500, nanos: 5_000 });
  expect(feed.observed.at(-1)).toEqual([500, 5_000, true]);
  feed.receiver.close();
});

test("feed refusals back off as busy until the feed opens, an unknown id rotates, and the marker asks for sign-in", async () => {
  const refusal = (status: number, code: string, retryAfter?: string) => () =>
    new Response("refused", {
      status,
      headers: { "X-Graphite-Upload-Refusal": code, ...(retryAfter && { "Retry-After": retryAfter }) },
    });
  const feed = follow([
    () => {
      throw new TypeError("Failed to fetch");
    },
    refusal(503, "globalFull", "1"),
    refusal(429, "clientFull"),
    () => ndjson('{"type":"ready"}\n{"type":"error","code":"globalFull","message":"server full"}\n'),
    () => ndjson('{"type":"ready"}\n{"type":"error","code":"invalid","message":"unknown upload id"}\n'),
  ]);
  await settled(() => feed.endings.length === 5);
  expect(
    feed.endings.map(([{ reason, rotate, detail }, broken, delayMs]) => [reason, rotate, detail, broken, delayMs]),
  ).toEqual([
    ["connection-lost", undefined, "TypeError: Failed to fetch", true, 300],
    ["server-busy", undefined, "progress returned HTTP 503", false, 1_000],
    ["server-busy", undefined, "progress returned HTTP 429", false, 600],
    // The feed opened, ending the busy streak; the run decides the one replacement.
    ["server-busy", undefined, "server full", false, 300],
    ["protocol-error", true, "unknown upload id", false, 300],
  ]);
  expect(feed.receiver.busy).toBe(false);
  const ended = follow([
    () => {
      throw new SignInRequired({ id: "peer", url: "https://peer.example", name: "Peer" });
    },
  ]);
  await settled(ended.signedIn);
  expect(ended.endings).toEqual([]);
});

test("an HTTP feed silent for 1.5 s reconnects, while heartbeats keep a quiet one open", async () => {
  jest.useFakeTimers();
  try {
    const streams: ReadableStreamDefaultController<Uint8Array>[] = [];
    // As a browser's, the answer's body errors when its request is aborted.
    const open = ({ signal }: RequestInit) =>
      new Response(
        new ReadableStream({
          start(controller) {
            streams.push(controller);
            signal!.addEventListener("abort", () => controller.error(signal!.reason));
          },
        }),
      );
    const feed = follow([open, open]);
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
      [{ reason: "connection-lost", retry: true, detail: "progress stream went silent" }, false, 300],
    ]);
    expect(streams).toHaveLength(2);
    feed.receiver.close();
  } finally {
    jest.useRealTimers();
  }
});

test("a session's id is backed up by a DELETE at finish, unless a terminal record that passed the guard arrived", async () => {
  const progress = { type: "progress", bytes: 100, nanos: 1_000 } as const;
  const stale = follow([], [], true);
  stale.receiver.record(progress);
  // A regressing terminal record is no evidence that the id finalized.
  stale.receiver.record({ type: "complete", bytes: 50, nanos: 500 });
  await stale.receiver.finish();
  expect(stale.deletes).toEqual([
    { method: "DELETE", cache: "no-store", keepalive: true, signal: expect.any(AbortSignal) },
  ]);
  const complete = follow([], [], true);
  complete.receiver.record(progress);
  complete.receiver.record({ ...progress, type: "complete" });
  await complete.receiver.finish();
  expect(complete.deletes).toEqual([]);
});
