import { test, expect, afterEach, beforeEach, jest } from "bun:test";
import {
  bootWorker,
  fakeWebTransport,
  type FakeWebTransport,
  type WorkerRealm,
} from "./test-helpers.testutil";
import { elapse, taskTurn } from "../../test-helpers.testutil";
import type { LaneFailure } from "../contract";
import { PROGRESS_FINAL_GRACE_MS } from "../real/budgets";

const SESSION_URL = "https://meter.test/wt/upload?id=gmu_test";
const DOWNLOAD_URL = "https://meter.test/wt/download?bytes=1024";
const PROGRESS_URL = "https://meter.test/upload/progress?id=gmu_test";
const MINT_URL = "https://meter.test/wt/session";

const DATAGRAM_BYTES = 1200;
const DRAIN_BUDGET = 40;
type Out = {
  type: string;
  retry?: boolean;
  reason?: string;
  detail?: string;
  msg?: { type: string; n?: number; detail?: string } & Partial<LaneFailure>;
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
interface Setup {
  timing: Timing;
  dialRefuses: boolean;
  holdDial: boolean;
  mintRefuses: boolean;
  fetch?: typeof fetch;
}
let mints = 0;
let feed: FeedStream;
let flood: ReturnType<typeof fakeDatagrams>;
let transport: ReturnType<typeof fakeWebTransport>;
let realm: Realm | undefined;
const tokenOf = (url: string): string =>
  new URL(url).searchParams.get("token") ?? "";
const session = (): FakeWebTransport => transport.sessions.at(-1)!;
function refusal(record: object): void {
  const stream = new FeedStream();
  session().incoming(stream.readable);
  stream.push(record);
  stream.close();
}
function mintFetch(mintRefuses: boolean): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
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
      feed.finish();
      return new Response(null);
    }
    throw new Error(`unexpected fetch ${url}`);
  }) as typeof fetch;
}
beforeEach(() => jest.useFakeTimers());
afterEach(() => {
  realm?.restore();
  realm = undefined;
  jest.useRealTimers();
});
type Realm = WorkerRealm<Out>;
async function boot(
  configure: (setup: Setup) => void = () => {},
): Promise<Realm> {
  const setup: Setup = {
    timing: "macro",
    dialRefuses: false,
    holdDial: false,
    mintRefuses: false,
  };
  configure(setup);
  mints = 0;
  transport = fakeWebTransport((dialled) => {
    if (setup.dialRefuses) dialled.refuse(new Error("connect refused"));
    else if (!setup.holdDial) dialled.accept();
    flood = dialled.datagrams = fakeDatagrams(setup.timing, () =>
      jest.advanceTimersByTime(1),
    );
    feed = new FeedStream();
    dialled.incoming(feed.readable);
  });
  realm = await bootWorker<Out>("./wt-transfer-worker.ts", {
    WebTransport: transport.WebTransport,
    fetch: setup.fetch ?? mintFetch(setup.mintRefuses),
  });
  return realm;
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
  configure: (setup: Setup) => void = () => {},
): Promise<Realm> {
  const realm = await boot(configure);
  startTransfer(realm, options);
  return realm;
}
test.each(["up", "down"] as const)(
  "the datagram %s loop yields to its own message queue",
  async (dir) => {
    for (const timing of ["micro", "macro"] as const) {
      const realm = await boot((setup) => (setup.timing = timing));
      startTransfer(realm, { dir, lanes: 0, datagrams: true });
      await taskTurn();
      const packets = dir === "up" ? flood.writes : flood.reads;
      realm.send({ type: "stop" });
      expect(packets).toBeLessThan(DRAIN_BUDGET);
      realm.restore();
    }
  },
);
test("a progress feed that ends without a terminal record is reported", async () => {
  const realm = await bootTransfer();
  await taskTurn();
  feed.push({ type: "ready" });
  feed.push({ type: "progress", bytes: 100, nanos: 1 });
  feed.close();
  await taskTurn();

  expect(
    realm.posted
      .filter((msg) => msg.type === "upload-progress")
      .map((msg) => msg.msg?.type),
  ).toEqual(["open", "bytes"]);
  expect(errors(realm)).toEqual([
    {
      type: "error",
      reason: "connection-lost",
      retry: true,
      detail: "webtransport progress feed ended early",
    },
  ]);
});
test("a later upload refusal stream preserves its disposition", async () => {
  const realm = await bootTransfer();
  await taskTurn();
  feed.push({ type: "ready" });
  refusal({
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
      reason: "connection-lost",
      retry: false,
      rotate: true,
    },
  });
  expect(errors(realm)).toEqual([]);
});
test("a datagram size that collapses to zero is reported", async () => {
  const realm = await bootTransfer({ lanes: 0, datagrams: true });
  await taskTurn();
  flood.collapseAfter = 3;
  await elapse(20);

  expect(errors(realm)).toEqual([
    {
      type: "error",
      reason: "connection-lost",
      retry: true,
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
    (setup) => (setup.dialRefuses = true),
  );
  await taskTurn();
  startDownload(realm, mintUrl);
  await taskTurn();

  expect(transport.sessions).toHaveLength(2);
  expect(tokenOf(transport.sessions[1].url)).toBe(
    tokenOf(transport.sessions[0].url),
  );
  expect(mints).toBe(1);
});
test("a session that established never offers its token again", async () => {
  const mintUrl = "https://spent.meter.test/wt/session";
  const realm = await bootTransfer({ dir: "down", mint: { url: mintUrl } });
  await taskTurn();
  startDownload(realm, mintUrl);
  await taskTurn();

  expect(transport.sessions).toHaveLength(2);
  expect(tokenOf(transport.sessions[1].url)).not.toBe(
    tokenOf(transport.sessions[0].url),
  );
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
    (setup) => (setup.mintRefuses = true),
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
    const realm = await bootTransfer({ credentials, headers }, (setup) => {
      const serve = mintFetch(false);
      setup.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
        if (init?.method === "DELETE") cleanup = init;
        return serve(input, init);
      }) as typeof fetch;
    });
    await taskTurn();
    realm.send({ type: "stop" });
    await taskTurn();
    expect(cleanup).toBeDefined();
    expect(cleanup!.credentials).toBe(credentials);
    expect(cleanup!.headers).toEqual(headers);
    expect(cleanup!.redirect).toBe("error");
    realm.restore();
  }
});

test("stopping a pending WebTransport dial cannot publish late establishment", async () => {
  const realm = await bootTransfer({}, (setup) => (setup.holdDial = true));
  await taskTurn();
  const pendingSession = session();
  realm.send({ type: "stop" });
  expect(pendingSession.closes).toBe(1);
  pendingSession.accept();
  await taskTurn();
  expect(realm.posted.map((message) => message.type)).toEqual(["stopped"]);
  expect(pendingSession.lanesOpened).toBe(0);
  expect(pendingSession.incomingUnidirectionalStreams.locked).toBe(false);
});

test("a stop whose progress feed never completes is acknowledged after the final grace", async () => {
  const realm = await bootTransfer({}, (setup) => {
    const serve = mintFetch(false);
    setup.fetch = (async (input: RequestInfo | URL, init?: RequestInit) =>
      init?.method === "DELETE"
        ? new Response(null)
        : serve(input, init)) as typeof fetch;
  });
  await taskTurn();
  feed.push({ type: "ready" });
  realm.send({ type: "stop" });
  await elapse(PROGRESS_FINAL_GRACE_MS - 5);
  expect(realm.posted.map((msg) => msg.type)).not.toContain("stopped");
  await elapse(5);
  expect(realm.posted.at(-1)?.type).toBe("stopped");
});
