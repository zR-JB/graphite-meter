import { test, expect, afterEach } from "bun:test";
import { bootWorker, type WorkerRealm } from "./test-helpers.test";

const globals = globalThis as Record<string, unknown>;

type Outcome = "accept" | "refuse" | "pending";

const park = (): Promise<void> => new Promise(() => {});

class Scenario {
  mints = 0;
  readonly dials: string[] = [];
  readonly sessions: FakeSession[] = [];
  outcomes: Outcome[] = ["accept"];
  halted = false;

  constructor(readonly id: string) {
    scenarios.set(id, this);
  }

  get pingUrl(): string {
    return `https://meter.test/${this.id}/wt/ping`;
  }

  get mintUrl(): string {
    return `https://meter.test/${this.id}/wt/token`;
  }

  outcomeFor(index: number): Outcome {
    return this.outcomes[Math.min(index, this.outcomes.length - 1)];
  }

  tokens(): string[] {
    return this.dials.map(
      (url) => new URL(url).searchParams.get("token") ?? "",
    );
  }
}

const scenarios = new Map<string, Scenario>();
const scenarioOf = (url: string): Scenario => {
  const id = new URL(url).pathname.split("/")[1];
  const found = scenarios.get(id);
  if (!found) throw new Error(`unexpected url ${url}`);
  return found;
};

class FakeSession {
  readonly ready: Promise<void>;
  readonly closed: Promise<void>;
  readonly datagrams = {
    writable: new WritableStream<Uint8Array>({ write: () => {} }),
    readable: new ReadableStream<Uint8Array>({ pull: park }),
  };
  #drop: () => void = () => {};

  constructor(url: string) {
    const scenario = scenarioOf(url);
    const outcome = scenario.outcomeFor(scenario.dials.length);
    scenario.dials.push(url);
    scenario.sessions.push(this);
    if (outcome === "refuse") {
      const err = new Error("connect refused");
      this.ready = this.closed = Promise.reject(err);
      return;
    }
    if (outcome === "pending") {
      this.ready = park();
      this.closed = new Promise<void>((resolve) => {
        this.#drop = resolve;
      });
      return;
    }
    this.ready = Promise.resolve();
    this.closed = new Promise<void>((resolve) => {
      this.#drop = resolve;
    });
  }

  drop(): void {
    this.#drop();
  }

  close(): void {
    this.#drop();
  }
}

const realFetch = globalThis.fetch;
const realPost = globals.postMessage;
const realWebTransport = globals.WebTransport;

const fakeFetch = async (input: RequestInfo | URL): Promise<Response> => {
  const url = String(input);
  const scenario = scenarioOf(url);
  if (url !== scenario.mintUrl) throw new Error(`unexpected fetch ${url}`);
  if (scenario.halted)
    return new Response("", {
      status: 403,
      headers: { "Graphite-Meter-Auth": "required" },
    });
  scenario.mints++;
  return Response.json({
    token: `tok-${scenario.id}-${scenario.mints}`,
    expires: Date.now() + 30_000,
  });
};

let realms = 0;

type Realm = WorkerRealm<{ type: string }>;

async function boot(): Promise<Realm> {
  return bootWorker("./ping-worker.ts", realms++);
}

async function start(scenario: Scenario): Promise<Realm> {
  globals.WebTransport = FakeSession;
  globalThis.fetch = fakeFetch as typeof fetch;
  const realm = await boot();
  realm.send({
    type: "start",
    url: scenario.pingUrl,
    transport: "webtransport",
    mint: { url: scenario.mintUrl },
    intervalMs: 250,
    replyDriven: false,
    maxInFlight: 16,
    reportGapMs: 20,
    lossK: 4,
    lossFloorMs: 250,
    checkAuthentication: true,
  });
  return realm;
}

async function halt(scenario: Scenario): Promise<void> {
  scenario.halted = true;
  await Bun.sleep(250);
}

afterEach(() => {
  globalThis.fetch = realFetch;
  globals.postMessage = realPost;
  globalThis.onmessage = null;
  if (realWebTransport === undefined)
    Reflect.deleteProperty(globals, "WebTransport");
  else globals.WebTransport = realWebTransport;
});

// Only a CONNECT the server accepted spends a token; a dial that dies before that leaves it valid, and re-minting for.
test("a dial refused before acceptance re-dials on the same token", async () => {
  const scenario = new Scenario("refused");
  scenario.outcomes = ["refuse"];
  await start(scenario);
  await Bun.sleep(200);
  await halt(scenario);

  expect(scenario.dials.length).toBeGreaterThanOrEqual(2);
  expect(new Set(scenario.tokens()).size).toBe(1);
  expect(scenario.mints).toBe(1);
});

// The server deletes a token on the CONNECT that carries it, so offering it again is a replay it refuses.
test("a dial the server accepted never offers its token again", async () => {
  const scenario = new Scenario("accepted");
  scenario.outcomes = ["accept"];
  await start(scenario);
  await Bun.sleep(50);
  expect(scenario.dials.length).toBe(1);

  scenario.sessions[0].drop();
  await Bun.sleep(250);
  await halt(scenario);

  expect(scenario.dials.length).toBeGreaterThanOrEqual(2);
  expect(scenario.tokens()[1]).not.toBe(scenario.tokens()[0]);
  expect(scenario.mints).toBeGreaterThanOrEqual(2);
});

test("a pending dial times out and retries with the same unspent token", async () => {
  const scenario = new Scenario("pending");
  scenario.outcomes = ["pending", "accept"];
  await start(scenario);
  await Bun.sleep(3_300);

  expect(scenario.dials).toHaveLength(2);
  expect(scenario.tokens()[1]).toBe(scenario.tokens()[0]);
  expect(scenario.mints).toBe(1);

  scenario.halted = true;
  scenario.sessions.at(-1)?.drop();
  await Bun.sleep(250);
}, 10_000);
