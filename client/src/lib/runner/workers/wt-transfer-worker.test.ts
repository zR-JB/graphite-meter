import { test, expect, afterEach, beforeEach, jest } from "bun:test";
import {
  bootWorker,
  elapse,
  taskTurn,
  type WorkerRealm,
} from "./test-helpers.testutil";

const globals = globalThis as Record<string, unknown>;
const SESSION_URL = "https://meter.test/wt/upload?id=gmu_test";
const DOWNLOAD_URL = "https://meter.test/wt/download?bytes=1024";
const PROGRESS_URL = "https://meter.test/upload/progress?id=gmu_test";
const MINT_URL = "https://meter.test/wt/session";

const DATAGRAM_BYTES = 1200;
const DRAIN_BUDGET = 40;
type Out = {
  type: string;
  recoverable?: boolean;
  detail?: string;
  msg?: { type: string; n?: number; detail?: string; cause?: string };
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
      headers?: Record<string, string>;
      credentials?: RequestCredentials;
    }
  | { type: "stop" };

type Timing = "micro" | "macro";
const park = (): Promise<void> => new Promise(() => {});

class FeedStream {
  #controller!: ReadableStreamDefaultController<Uint8Array>;
  readonly readable = new ReadableStream<Uint8Array>({
    start: (controller) => (this.#controller = controller),
  });
  push(record: object): void {
    this.#controller.enqueue(
      new TextEncoder().encode(`${JSON.stringify(record)}\n`),
    );
  }
  close(): void {
    this.#controller.close();
  }
  finish(): void {
    this.push({ type: "complete", bytes: 0, nanos: 1 });
    this.close();
  }
}
function fakeDatagrams(timing: Timing, tick: () => void) {
  let writes = 0;
  let reads = 0;
  let collapseAfter = Infinity;
  const turn = (): Promise<void> | undefined =>
    timing === "macro" ? taskTurn() : undefined;
  return {
    get writes() {
      return writes;
    },
    get reads() {
      return reads;
    },
    get collapseAfter() {
      return collapseAfter;
    },
    set collapseAfter(value: number) {
      collapseAfter = value;
    },
    writable: new WritableStream<Uint8Array>({
      write: () => {
        writes++;
        tick();
        return writes >= DRAIN_BUDGET ? park() : turn();
      },
    }),
    readable: new ReadableStream<Uint8Array>({
      pull: (controller) => {
        reads++;
        tick();
        if (reads >= DRAIN_BUDGET) return park();
        controller.enqueue(new Uint8Array(DATAGRAM_BYTES));
        return turn();
      },
    }),
    get maxDatagramSize() {
      return writes >= collapseAfter ? 0 : DATAGRAM_BYTES;
    },
  };
}
let timing: Timing = "macro";
let mintRefuses = false;
let dialRefuses = false;
let dialReady: Promise<void> | undefined;
let mints = 0;
const dialUrls: string[] = [];
const tokenOf = (url: string): string =>
  new URL(url).searchParams.get("token") ?? "";
const dialed: FakeSession[] = [];
class FakeSession {
  readonly ready =
    dialReady ??
    (dialRefuses
      ? Promise.reject(new Error("connect refused"))
      : Promise.resolve());
  end!: (info: WebTransportCloseInfo) => void;
  readonly closed = new Promise<WebTransportCloseInfo>(
    (resolve) => (this.end = resolve),
  );
  readonly datagrams = fakeDatagrams(timing, () => jest.advanceTimersByTime(1));
  readonly feed = new FeedStream();
  closes = 0;
  lanes = 0;
  #incoming!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;
  readonly incomingUnidirectionalStreams = new ReadableStream<
    ReadableStream<Uint8Array>
  >({
    start: (controller) => (this.#incoming = controller),
  });
  constructor(url = SESSION_URL) {
    this.#incoming.enqueue(this.feed.readable);
    dialUrls.push(url);
    dialed.push(this);
  }
  refusal(record: object): void {
    const stream = new FeedStream();
    this.#incoming.enqueue(stream.readable);
    stream.push(record);
    stream.close();
  }
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
const fakeFetch = async (
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> => {
  const url = String(input);
  if (new URL(url).pathname === "/wt/session") {
    if (mintRefuses)
      return new Response("", {
        status: 403,
        headers: { "Graphite-Meter-Auth": "required" },
      });
    mints++;
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
  dialReady = undefined;
  mints = 0;
  dialUrls.length = 0;
  dialed.length = 0;
  globals.WebTransport = FakeSession;
  globalThis.fetch = fakeFetch as typeof fetch;
}
beforeEach(() => jest.useFakeTimers());
afterEach(() => {
  jest.useRealTimers();
  globalThis.fetch = realFetch;
  globals.postMessage = realPost;
  globalThis.onmessage = null;
  if (realWebTransport === undefined)
    Reflect.deleteProperty(globals, "WebTransport");
  else globals.WebTransport = realWebTransport;
});
type Realm = WorkerRealm<Out>;
let realms = 0;
async function boot(): Promise<Realm> {
  return bootWorker("./wt-transfer-worker.ts", realms++);
}
const errors = (realm: Realm): Out[] =>
  realm.posted.filter((msg) => msg.type === "error");

type Start = Extract<In, { type: "start" }>;
function startTransfer(
  realm: Realm,
  options: Partial<Omit<Start, "type">> = {},
): void {
  realm.send({
    type: "start",
    url: options.dir === "down" ? DOWNLOAD_URL : SESSION_URL,
    dir: "up",
    lanes: 1,
    datagrams: false,
    progressUrl: PROGRESS_URL,
    ...options,
  });
}
async function bootTransfer(
  options: Partial<Omit<Start, "type">> = {},
  configure: () => void = () => {},
): Promise<Realm> {
  install();
  configure();
  const realm = await boot();
  startTransfer(realm, options);
  return realm;
}
async function packetsBeforeStopSeen(
  dir: "up" | "down",
  sinkTiming: Timing,
): Promise<number> {
  install(sinkTiming);
  const realm = await boot();
  startTransfer(realm, { dir, lanes: 0, datagrams: true });
  await taskTurn();
  const { writes, reads } = session().datagrams;
  realm.send({ type: "stop" });
  return dir === "up" ? writes : reads;
}
test("the datagram upload loop yields to its own message queue", async () => {
  const micro = await packetsBeforeStopSeen("up", "micro");
  const macro = await packetsBeforeStopSeen("up", "macro");

  expect(micro).toBeLessThan(DRAIN_BUDGET);
  expect(macro).toBeLessThan(DRAIN_BUDGET);
});
test("the datagram download loop yields to its own message queue", async () => {
  const micro = await packetsBeforeStopSeen("down", "micro");
  const macro = await packetsBeforeStopSeen("down", "macro");

  expect(micro).toBeLessThan(DRAIN_BUDGET);
  expect(macro).toBeLessThan(DRAIN_BUDGET);
});
test("a progress feed that ends without a terminal record is reported", async () => {
  const realm = await bootTransfer();
  await taskTurn();
  session().feed.push({ type: "ready" });
  session().feed.push({ type: "progress", bytes: 100, nanos: 1 });
  session().feed.close();
  await taskTurn();

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
test("a later upload refusal stream preserves its structural cause", async () => {
  const realm = await bootTransfer();
  await taskTurn();
  session().feed.push({ type: "ready" });
  session().refusal({
    type: "error",
    code: "invalid",
    message: "unknown upload id",
  });
  await taskTurn();

  expect(
    realm.posted.filter((msg) => msg.type === "upload-progress").at(-1),
  ).toEqual({
    type: "upload-progress",
    msg: {
      type: "fatal",
      detail: "unknown upload id",
      cause: "unknown-upload-id",
    },
  });
  expect(errors(realm)).toEqual([]);
});
test("a datagram size that collapses to zero is reported", async () => {
  const realm = await bootTransfer({ lanes: 0, datagrams: true });
  await taskTurn();
  session().datagrams.collapseAfter = 3;
  await elapse(20);

  expect(errors(realm)).toEqual([
    {
      type: "error",
      recoverable: true,
      detail: "webtransport datagram size collapsed",
    },
  ]);
});
function startDownload(realm: Realm, mintUrl: string): void {
  startTransfer(realm, { dir: "down", mint: { url: mintUrl } });
}
test("a dial refused before acceptance re-dials on the same token", async () => {
  const mintUrl = "https://unspent.meter.test/wt/session";
  const realm = await bootTransfer(
    { dir: "down", mint: { url: mintUrl } },
    () => (dialRefuses = true),
  );
  await taskTurn();
  startDownload(realm, mintUrl);
  await taskTurn();

  expect(dialUrls.length).toBe(2);
  expect(tokenOf(dialUrls[1])).toBe(tokenOf(dialUrls[0]));
  expect(mints).toBe(1);
});
test("a session that established never offers its token again", async () => {
  const mintUrl = "https://spent.meter.test/wt/session";
  const realm = await bootTransfer({ dir: "down", mint: { url: mintUrl } });
  await taskTurn();
  startDownload(realm, mintUrl);
  await taskTurn();

  expect(dialUrls.length).toBe(2);
  expect(tokenOf(dialUrls[1])).not.toBe(tokenOf(dialUrls[0]));
  expect(mints).toBe(2);
});
test("a session the server revokes asks for sign-in instead of redialling", async () => {
  const realm = await bootTransfer({ dir: "down" });
  await taskTurn();
  session().end({ closeCode: 3, reason: "authentication required" });
  await taskTurn();

  expect(realm.posted.map((msg) => msg.type)).toEqual([
    "established",
    "auth-required",
  ]);
});
test("a stop after auth-required is still acknowledged", async () => {
  const realm = await bootTransfer(
    { mint: { url: MINT_URL } },
    () => (mintRefuses = true),
  );
  await taskTurn();
  expect(realm.posted.map((msg) => msg.type)).toEqual(["auth-required"]);

  realm.send({ type: "stop" });
  await taskTurn();

  expect(realm.posted.map((msg) => msg.type)).toEqual([
    "auth-required",
    "stopped",
  ]);
});

test("authenticated WebTransport upload cleanup refuses redirects for session and grant requests", async () => {
  for (const credentials of ["include", "omit"] as const) {
    let cleanup: RequestInit | undefined;
    const headers: Record<string, string> =
      credentials === "include"
        ? { "X-CSRF-Token": "session-csrf" }
        : { Authorization: "Bearer peer-grant" };
    const realm = await bootTransfer({ credentials, headers }, () => {
      globalThis.fetch = (async (
        input: RequestInfo | URL,
        init?: RequestInit,
      ) => {
        if (init?.method === "DELETE") cleanup = init;
        return fakeFetch(input, init);
      }) as typeof fetch;
    });
    await taskTurn();
    realm.send({ type: "stop" });
    await taskTurn();
    expect(cleanup).toBeDefined();
    expect(cleanup!.credentials).toBe(credentials);
    expect(cleanup!.headers).toEqual(headers);
    expect(cleanup!.redirect).toBe("error");
  }
});

test("stopping a pending WebTransport dial cannot publish late establishment", async () => {
  let ready!: () => void;
  const realm = await bootTransfer({}, () => {
    dialReady = new Promise<void>((resolve) => {
      ready = resolve;
    });
  });
  await taskTurn();
  const pendingSession = session();
  realm.send({ type: "stop" });
  expect(pendingSession.closes).toBe(1);
  ready();
  await taskTurn();
  expect(realm.posted.map((message) => message.type)).toEqual(["stopped"]);
  expect(pendingSession.lanes).toBe(0);
  expect(pendingSession.incomingUnidirectionalStreams.locked).toBe(false);
});
