import { test, expect, afterEach } from "bun:test";

/* Drives the real worker module against a fake WebTransport. The ping bus
 * re-dials inside its own realm, so the module-scope token cache is live across
 * its reconnects the way it never is for the transfer worker, which gets a
 * fresh worker -- and so a fresh realm -- per restart. Each scenario therefore
 * owns its URLs: the cache is module state shared by every realm this file
 * boots, and only a distinct URL keeps one scenario's held token out of the
 * next one's. */

const globals = globalThis as Record<string, unknown>;

/** How a dial settles. `refuse` is a CONNECT the server never accepted, so its
 *  token is still unspent; `accept` is one the server consumed. */
type Outcome = "accept" | "refuse";

const park = (): Promise<void> => new Promise(() => {});

class Scenario {
  mints = 0;
  /** Session URLs in dial order, token query and all. */
  readonly dials: string[] = [];
  readonly sessions: FakeSession[] = [];
  /** Settlement for each dial in turn; later dials repeat the last entry. */
  outcomes: Outcome[] = ["accept"];
  /** Latches the realm: a mint refusal carrying the auth marker is the one
   *  answer that stops the worker re-dialling for the rest of the file. */
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

  /** The token each dial carried. */
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
    // The bus never answers: no frame is needed to prove which token dialled.
    readable: new ReadableStream<Uint8Array>({ pull: park }),
  };
  #drop: () => void = () => {};

  constructor(url: string) {
    const scenario = scenarioOf(url);
    const outcome = scenario.outcomeFor(scenario.dials.length);
    scenario.dials.push(url);
    scenario.sessions.push(this);
    if (outcome === "refuse") {
      // A dial the server never accepted rejects both promises, and the worker
      // attaches to both in the same turn, so neither goes unhandled.
      const err = new Error("connect refused");
      this.ready = Promise.reject(err);
      this.closed = Promise.reject(err);
      return;
    }
    this.ready = Promise.resolve();
    this.closed = new Promise<void>((resolve) => {
      this.#drop = resolve;
    });
  }

  /** The server closing an accepted session, which is what makes the ping bus
   *  re-dial in the realm it is already running in. */
  drop(): void {
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

interface Realm {
  posted: { type: string }[];
  send(msg: object): void;
}

async function boot(): Promise<Realm> {
  const posted: { type: string }[] = [];
  globals.postMessage = (msg: { type: string }): void => {
    posted.push(msg);
  };
  await import(`./ping-worker.ts?realm=${realms++}`);
  const handler = globalThis.onmessage as (event: MessageEvent) => void;
  return {
    posted,
    send: (msg) => handler({ data: msg } as MessageEvent),
  };
}

/** Boot a realm already dialling the scenario's bus over WebTransport. */
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

/** Stop a realm re-dialling for the rest of the file. A ping worker has no
 *  shutdown message -- the owner terminates it -- so the only latch a test can
 *  reach is the terminal auth refusal. */
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

// Only a CONNECT the server accepted spends a token; a dial that dies before
// that leaves it valid, and re-minting for the retry parks a second token
// against the session cap that every stage and tab of the same login draws on.
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

// The server deletes a token on the CONNECT that carries it, so offering it
// again is a replay it refuses. In this worker the re-dial happens in the realm
// that holds the cache, so an unreported spend fails every reconnect for the
// whole reuse window instead of reconnecting on the first attempt.
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
