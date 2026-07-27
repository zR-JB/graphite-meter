import { test, expect, afterEach } from "bun:test";

/* Drives the real worker module against a fake WebTransport. The worker holds
 * its session state at module scope and installs its handler on the global, so
 * a scenario imports its own instance: that is what a spawn gives it. */

const globals = globalThis as Record<string, unknown>;

const SESSION_URL = "https://meter.test/wt/upload?id=gmu_test";
const PROGRESS_URL = "https://meter.test/upload/progress/gmu_test";
const MINT_URL = "https://meter.test/wt/token";

const DATAGRAM_BYTES = 1200;
/** Packets the fake transport drains before it stops draining. A worker that
 *  never takes a task turn spins its microtask queue for as long as the
 *  transport keeps settling, which reads as a hung suite rather than a
 *  failure; the budget turns it back into an assertion. */
const DRAIN_BUDGET = 40;

type Out = {
  type: string;
  recoverable?: boolean;
  detail?: string;
  msg?: { type: string; n?: number };
};

type In =
  | {
      type: "start";
      url: string;
      dir: "down" | "up";
      lanes: number;
      datagrams: boolean;
      mint?: { url: string };
      progressUrl?: string;
    }
  | { type: "stop" };

/** How the transport settles a write or a read. `micro` settles inside the
 *  microtask checkpoint the worker is already in, which is what a default
 *  WritableStream sink does; `macro` settles on a task, as a real one does. */
type Timing = "micro" | "macro";

const park = (): Promise<void> => new Promise(() => {});
const macroTurn = (): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve));

/** One server-opened NDJSON stream. */
class FeedStream {
  #controller!: ReadableStreamDefaultController<Uint8Array>;
  readonly readable = new ReadableStream<Uint8Array>({
    start: (controller) => {
      this.#controller = controller;
    },
  });

  push(record: object): void {
    this.#controller.enqueue(
      new TextEncoder().encode(`${JSON.stringify(record)}\n`),
    );
  }

  close(): void {
    this.#controller.close();
  }

  /** What the server does when the finalizing DELETE lands. */
  finish(): void {
    this.push({ type: "complete", bytes: 0, nanos: 1 });
    this.close();
  }
}

class FakeDatagrams {
  writes = 0;
  reads = 0;
  /** Writes after which the path MTU estimate collapses to zero, as a shrinking
   *  estimate reaches the worker. */
  collapseAfter = Infinity;
  readonly writable: WritableStream<Uint8Array>;
  readonly readable: ReadableStream<Uint8Array>;

  constructor(timing: Timing, tick: () => void) {
    this.writable = new WritableStream<Uint8Array>({
      write: () => {
        this.writes++;
        tick();
        if (this.writes >= DRAIN_BUDGET) return park();
        return timing === "macro" ? macroTurn() : undefined;
      },
    });
    this.readable = new ReadableStream<Uint8Array>({
      pull: (controller) => {
        this.reads++;
        tick();
        if (this.reads >= DRAIN_BUDGET) return park();
        controller.enqueue(new Uint8Array(DATAGRAM_BYTES));
        return timing === "macro" ? macroTurn() : undefined;
      },
    });
  }

  get maxDatagramSize(): number {
    return this.writes >= this.collapseAfter ? 0 : DATAGRAM_BYTES;
  }
}

/** The settle timing the next dial's datagrams will use. */
let timing: Timing = "macro";
/** Whether the mint refuses this dial as unauthenticated. */
let mintRefuses = false;
/** Whether a dial dies before its CONNECT is accepted, which leaves the token
 *  it carried unspent. */
let dialRefuses = false;
/** Mints answered since the last install. */
let mints = 0;
/** Session URLs in dial order, token query and all. */
const dialUrls: string[] = [];
const tokenOf = (url: string): string =>
  new URL(url).searchParams.get("token") ?? "";
/** Virtual milliseconds the transport has consumed. Real time barely moves
 *  across a microtask spin, so the transport advances the clock the worker
 *  reads and a time-bounded yield becomes deterministic. */
let clockMs = 0;
const dialed: FakeSession[] = [];

class FakeSession {
  // A refused dial rejects in the turn the worker races it in, so it is never
  // an unhandled rejection.
  readonly ready = dialRefuses
    ? Promise.reject(new Error("connect refused"))
    : Promise.resolve();
  /** Never settles: the worker treats either arm as this session's death. */
  readonly closed = park();
  readonly datagrams = new FakeDatagrams(timing, () => {
    clockMs += 1;
  });
  readonly feed = new FeedStream();
  closes = 0;
  lanes = 0;
  #incoming!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;
  readonly incomingUnidirectionalStreams = new ReadableStream<
    ReadableStream<Uint8Array>
  >({
    start: (controller) => {
      this.#incoming = controller;
    },
  });

  constructor(url = SESSION_URL) {
    // The server opens the progress feed as the first incoming stream.
    this.#incoming.enqueue(this.feed.readable);
    dialUrls.push(url);
    dialed.push(this);
  }

  /** An upload lane whose sink never drains: a real one parks on the
   *  transport's queue rather than looping on a settled write. */
  createUnidirectionalStream(): Promise<WritableStream<Uint8Array>> {
    this.lanes++;
    return Promise.resolve(new WritableStream<Uint8Array>({ write: park }));
  }

  close(): void {
    this.closes++;
  }
}

const session = (): FakeSession => dialed[dialed.length - 1];

const realFetch = globalThis.fetch;
const realPost = globals.postMessage;
const realWebTransport = globals.WebTransport;
const realNow = performance.now.bind(performance);

const fakeFetch = async (
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> => {
  const url = String(input);
  if (url.endsWith("/wt/token")) {
    if (mintRefuses)
      return new Response("", {
        status: 403,
        headers: { "Graphite-Meter-Auth": "required" },
      });
    mints++;
    // A distinct token per mint is what tells a reused one from a fresh one;
    // the expiry is what makes it reusable at all.
    return Response.json({
      token: `mint-${mints}`,
      expires: Date.now() + 30_000,
    });
  }
  if (url === PROGRESS_URL && init?.method === "DELETE") {
    session().feed.finish();
    return new Response(null);
  }
  throw new Error(`unexpected fetch ${url}`);
};

function install(sinkTiming: Timing = "macro"): void {
  timing = sinkTiming;
  mintRefuses = false;
  dialRefuses = false;
  mints = 0;
  clockMs = 0;
  dialUrls.length = 0;
  dialed.length = 0;
  globals.WebTransport = FakeSession;
  globalThis.fetch = fakeFetch as typeof fetch;
  performance.now = () => realNow() + clockMs;
}

afterEach(() => {
  performance.now = realNow;
  globalThis.fetch = realFetch;
  globals.postMessage = realPost;
  globalThis.onmessage = null;
  if (realWebTransport === undefined)
    Reflect.deleteProperty(globals, "WebTransport");
  else globals.WebTransport = realWebTransport;
});

interface Realm {
  posted: Out[];
  send(msg: In): void;
}

let realms = 0;

async function boot(): Promise<Realm> {
  const posted: Out[] = [];
  globals.postMessage = (msg: Out): void => {
    posted.push(msg);
  };
  await import(`./wt-transfer-worker.ts?realm=${realms++}`);
  const handler = globalThis.onmessage as (event: MessageEvent) => void;
  return {
    posted,
    send: (msg) => handler({ data: msg } as MessageEvent),
  };
}

const errors = (realm: Realm): Out[] =>
  realm.posted.filter((msg) => msg.type === "error");

/** Start an upload session and queue a `stop` as a task before the transfer
 *  loop can run, the way the owner's stop reaches a busy worker. Reports how
 *  many packets the transport had moved by the time the loop saw it. */
async function packetsBeforeStopSeen(
  dir: "up" | "down",
  sinkTiming: Timing,
): Promise<number> {
  install(sinkTiming);
  const realm = await boot();
  realm.send({
    type: "start",
    url: SESSION_URL,
    dir,
    lanes: 0,
    datagrams: true,
    progressUrl: PROGRESS_URL,
  });
  let seen = -1;
  setTimeout(() => {
    seen =
      dir === "up" ? session().datagrams.writes : session().datagrams.reads;
    realm.send({ type: "stop" });
  });
  await Bun.sleep(30);
  return seen;
}

// `await writer.ready` is the loop's only suspension, and a sink that settles
// a write inside the same microtask checkpoint resolves it without ever
// reaching a task. A worker's `message` events are dispatched only on a task,
// so a loop with no unconditional yield outruns the queue carrying its own
// `stop` and keeps writing until the transport stops draining.
test("the datagram upload loop yields to its own message queue", async () => {
  const micro = await packetsBeforeStopSeen("up", "micro");
  const macro = await packetsBeforeStopSeen("up", "macro");

  expect(micro).toBeLessThan(DRAIN_BUDGET);
  expect(macro).toBeLessThan(DRAIN_BUDGET);
});

// `await reader.read()` has the same shape: a source that enqueues inside the
// checkpoint settles the read without a task turn.
test("the datagram download loop yields to its own message queue", async () => {
  const micro = await packetsBeforeStopSeen("down", "micro");
  const macro = await packetsBeforeStopSeen("down", "macro");

  expect(micro).toBeLessThan(DRAIN_BUDGET);
  expect(macro).toBeLessThan(DRAIN_BUDGET);
});

// A feed that ends without a terminal record is a dropped feed, not a finished
// upload: the stream resolves rather than throwing, so the transport-break path
// never sees it. Left silent, the stage counts no further server bytes, reports
// no stall and completes on whatever it had already counted.
test("a progress feed that ends without a terminal record is reported", async () => {
  install();
  const realm = await boot();
  realm.send({
    type: "start",
    url: SESSION_URL,
    dir: "up",
    lanes: 1,
    datagrams: false,
    progressUrl: PROGRESS_URL,
  });
  await Bun.sleep(5);
  session().feed.push({ type: "ready" });
  session().feed.push({ type: "progress", bytes: 100, nanos: 1 });
  session().feed.close();
  await Bun.sleep(5);

  expect(
    realm.posted
      .filter((msg) => msg.type === "upload-progress")
      .map((msg) => msg.msg?.type),
  ).toEqual(["open", "bytes"]);
  expect(errors(realm)).toEqual([
    {
      type: "error",
      recoverable: true,
      detail: "webtransport progress feed ended early",
    },
  ]);
});

// A path MTU that collapses mid-session leaves nothing to send. Ending the
// upload silently leaves the stage running to its full timer with zero bytes
// and no diagnostic, because no failure ever reaches the lane's owner.
test("a datagram size that collapses to zero is reported", async () => {
  install();
  const realm = await boot();
  realm.send({
    type: "start",
    url: SESSION_URL,
    dir: "up",
    lanes: 0,
    datagrams: true,
    progressUrl: PROGRESS_URL,
  });
  await Bun.sleep(0);
  session().datagrams.collapseAfter = 3;
  await Bun.sleep(20);

  expect(errors(realm)).toEqual([
    {
      type: "error",
      recoverable: true,
      detail: "webtransport datagram size collapsed",
    },
  ]);
});

/** Start a download session on its own mint URL. The token cache is module
 *  state shared by every realm this file boots, so a scenario that holds a
 *  token has to key it somewhere no other test reads. */
function startDownload(realm: Realm, mintUrl: string): void {
  realm.send({
    type: "start",
    url: SESSION_URL,
    dir: "down",
    lanes: 1,
    datagrams: false,
    mint: { url: mintUrl },
  });
}

// Only a CONNECT the server accepted spends a token. A dial that died before
// that left it valid, so the retry carries the same one rather than parking a
// second token against the session cap every stage and tab of the login draws
// on.
test("a dial refused before acceptance re-dials on the same token", async () => {
  install();
  dialRefuses = true;
  const mintUrl = "https://meter.test/unspent/wt/token";
  const realm = await boot();
  startDownload(realm, mintUrl);
  await Bun.sleep(5);
  startDownload(realm, mintUrl);
  await Bun.sleep(5);

  expect(dialUrls.length).toBe(2);
  expect(tokenOf(dialUrls[1])).toBe(tokenOf(dialUrls[0]));
  expect(mints).toBe(1);
});

// The server deletes the token on the CONNECT that carries it, so a session
// that established has to report the spend: offering it again is a replay the
// server refuses. In production each restart is a fresh worker and so a fresh
// cache, which is exactly why nothing here would notice a missing report --
// this pins the module contract instead.
test("a session that established never offers its token again", async () => {
  install();
  const mintUrl = "https://meter.test/spent/wt/token";
  const realm = await boot();
  startDownload(realm, mintUrl);
  await Bun.sleep(5);
  startDownload(realm, mintUrl);
  await Bun.sleep(5);

  expect(dialUrls.length).toBe(2);
  expect(tokenOf(dialUrls[1])).not.toBe(tokenOf(dialUrls[0]));
  expect(mints).toBe(2);
});

// `auth-required` latches the same stop flag a graceful stop does, so the ack
// the owner waits on has to survive it: without one the stop burns the full
// grace before the worker is terminated.
test("a stop after auth-required is still acknowledged", async () => {
  install();
  mintRefuses = true;
  const realm = await boot();
  realm.send({
    type: "start",
    url: SESSION_URL,
    dir: "up",
    lanes: 1,
    datagrams: false,
    progressUrl: PROGRESS_URL,
    mint: { url: MINT_URL },
  });
  await Bun.sleep(5);
  expect(realm.posted.map((msg) => msg.type)).toEqual(["auth-required"]);

  realm.send({ type: "stop" });
  await Bun.sleep(5);

  expect(realm.posted.map((msg) => msg.type)).toEqual([
    "auth-required",
    "stopped",
  ]);
});
