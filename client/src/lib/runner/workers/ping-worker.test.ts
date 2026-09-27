import { test, expect, afterEach, beforeEach, jest } from "bun:test";
import type { PingWorkerEvent } from "./pingSample";
import {
  bootWorker,
  fakeWebTransport,
  type FakeWebTransport,
  type WorkerRealm,
} from "./test-helpers.testutil";
import { elapse, until } from "../../test-helpers.testutil";

type Outcome = "accept" | "refuse" | "pending";
type Realm = WorkerRealm<PingWorkerEvent>;

class Scenario {
  mints = 0;
  outcomes: Outcome[] = ["accept"];
  refuseFirstTicket = false;
  halted = false;
  readonly transport = fakeWebTransport((session) => this.#open(session));
  readonly pingUrl: string;
  readonly mintUrl: string;

  constructor(readonly id: string) {
    this.pingUrl = `https://${id}.meter.test/wt/ping`;
    this.mintUrl = `https://${id}.meter.test/wt/session`;
  }

  get sessions(): FakeWebTransport[] {
    return this.transport.sessions;
  }

  tokens(): string[] {
    return this.sessions.map(
      ({ url }) => new URL(url).searchParams.get("token") ?? "",
    );
  }

  #open(session: FakeWebTransport): void {
    const dial = this.sessions.length - 1;
    const outcome =
      this.refuseFirstTicket &&
      new URL(session.url).searchParams.get("token") === `tok-${this.id}-1`
        ? "refuse"
        : this.outcomes[Math.min(dial, this.outcomes.length - 1)];
    if (outcome === "refuse") session.refuse(new Error("connect refused"));
    if (outcome === "accept") session.accept();
  }

  readonly fetch = async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);
    if (url !== this.mintUrl) throw new Error(`unexpected fetch ${url}`);
    if (this.halted)
      return new Response("", {
        status: 403,
        headers: { "Graphite-Meter-Auth": "required" },
      });
    this.mints++;
    return Response.json({
      token: `tok-${this.id}-${this.mints}`,
      expires: Date.now() + 30_000,
    });
  };
}

let realm: Realm | undefined;

async function start(scenario: Scenario): Promise<Realm> {
  realm = await bootWorker<PingWorkerEvent>("./ping-worker.ts", {
    WebTransport: scenario.transport.WebTransport,
    fetch: scenario.fetch,
  });
  realm.send({
    type: "start",
    url: scenario.pingUrl,
    transport: "webtransport",
    mint: { url: scenario.mintUrl },
    intervalMs: 250,
    replyDriven: false,
    maxInFlight: 16,
    deadlineK: 4,
    deadlineFloorMs: 250,
    checkAuthentication: true,
  });
  return realm;
}

async function halt(scenario: Scenario): Promise<void> {
  scenario.halted = true;
  await elapse(250);
}

const pings = (session: FakeWebTransport | undefined) =>
  session?.sent.some((message) => message.startsWith("PING,")) === true;
const samples = (realm: Realm) =>
  realm.posted.flatMap((event) =>
    event.type === "samples" ? event.samples : [],
  );

beforeEach(() => jest.useFakeTimers());
afterEach(async () => {
  realm?.send({
    type: "stop",
    cutoffEpochMs: performance.timeOrigin + performance.now(),
  });
  await elapse(1_000);
  realm?.restore();
  realm = undefined;
  jest.useRealTimers();
});

// A dial that never reaches authentication leaves its ticket reusable for a bounded retry.
test("a dial refused before acceptance re-dials on the same token", async () => {
  const scenario = new Scenario("refused");
  scenario.outcomes = ["refuse"];
  await start(scenario);
  await elapse(200);
  await halt(scenario);

  expect(scenario.sessions.length).toBeGreaterThanOrEqual(2);
  expect(new Set(scenario.tokens()).size).toBe(1);
  expect(scenario.mints).toBe(1);
});

// The server deletes a token on the CONNECT that carries it, so offering it again is a replay it refuses.
test("a dial the server accepted never offers its token again", async () => {
  const scenario = new Scenario("accepted");
  scenario.outcomes = ["accept"];
  await start(scenario);
  await elapse(50);
  expect(scenario.sessions.length).toBe(1);

  scenario.sessions[0].end();
  await elapse(250);
  await halt(scenario);

  expect(scenario.sessions.length).toBeGreaterThanOrEqual(2);
  expect(scenario.tokens()[1]).not.toBe(scenario.tokens()[0]);
  expect(scenario.mints).toBeGreaterThanOrEqual(2);
});

test("a pending dial times out and retries with the same unspent token", async () => {
  const scenario = new Scenario("pending");
  scenario.outcomes = ["pending", "accept"];
  await start(scenario);
  await elapse(3_300);

  expect(scenario.sessions).toHaveLength(2);
  expect(scenario.tokens()[1]).toBe(scenario.tokens()[0]);
  expect(scenario.mints).toBe(1);

  scenario.halted = true;
  scenario.sessions.at(-1)?.end();
  await elapse(250);
});

test("WebTransport reconnect ignores stale datagrams and retains fresh reply timing", async () => {
  const scenario = new Scenario("timing-reconnect");
  const realm = await start(scenario);
  await until(() => pings(scenario.sessions[0]), 2_000);
  const first = scenario.sessions[0];
  expect(first.sent[0]).toBe("PING,0");
  realm.send({ type: "measure" });
  await until(() => first.sent.includes("PING,1"), 2_000);

  first.datagram("PONG,1,0");
  await until(() => samples(realm).length === 1, 2_000);
  first.end();
  await until(() => pings(scenario.sessions[1]), 2_000);
  const fresh = scenario.sessions[1];
  const id = fresh.sent
    .find((message) => message.startsWith("PING,"))!
    .slice(5);

  first.datagram(`PONG,${id},0`);
  fresh.datagram(`PONG,${id},0`);
  await until(() => samples(realm).length === 2, 2_000);
  expect(samples(realm).map((sample) => sample.reflectorHandlingMs)).toEqual([
    0, 0,
  ]);
  expect(samples(realm).every((sample) => !sample.timedOut)).toBe(true);
});

test("a closed WebTransport dial becoming ready cannot restart the replacement bus", async () => {
  const scenario = new Scenario("stale-ready");
  scenario.outcomes = ["pending", "accept"];
  const realm = await start(scenario);
  await until(() => scenario.sessions.length === 1, 2_000);
  const first = scenario.sessions[0];
  first.end();
  await until(() => pings(scenario.sessions[1]), 2_000);
  const opens = () => realm.posted.filter((event) => event.type === "open");
  expect(opens()).toHaveLength(1);
  first.accept();
  await elapse(10);
  expect(first.sent).toEqual([]);
  expect(opens()).toHaveLength(1);
});

test("a ticket spent by a temporary downstream refusal can recover within readiness budget", async () => {
  const scenario = new Scenario("spent-before-upgrade");
  scenario.refuseFirstTicket = true;
  const realm = await start(scenario);
  await elapse(3_400);
  expect(realm.posted.some((message) => message.type === "open")).toBe(true);
  expect(scenario.mints).toBeGreaterThanOrEqual(2);
});
