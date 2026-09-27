import { test, expect, afterEach, beforeEach, jest } from "bun:test";
import {
  PING_STOP_MARGIN_MS,
  PING_TIMEOUT_CEIL_MS,
} from "../workers/pingSample";
import {
  IdleKeepalive,
  LatencyChannel,
  type IdleEvent,
} from "./latencyChannel";
import type { ParticipantHost } from "../transport";
import type { LatencyTarget } from "../../api/endpoints";
import { testWorkers } from "./test-helpers.testutil";
import { stubGlobals } from "../../test-helpers.testutil";
import { ServerAuthenticationRequired } from "../../servers/credentials";

const target: LatencyTarget = {
  id: "http://meter.test:7246",
  origin: "http://meter.test:7246",
  transport: "websocket",
  protocol: "http1",
  tls: false,
};
const credentials = {
  server: { id: "self", name: "Test server", url: target.origin },
  kind: "public" as const,
};

type ChannelHost = ConstructorParameters<typeof LatencyChannel>[0]["host"];
const host = (overrides: Partial<ParticipantHost>): ChannelHost => ({
  latency() {},
  latencyInterrupted() {},
  latencyIncomplete() {},
  stallLatency() {},
  resumeLatency() {},
  authenticationRequired() {},
  ...overrides,
});

let workers: ReturnType<typeof testWorkers>;
let restore: () => void;
beforeEach(() => {
  workers = testWorkers();
  restore = stubGlobals({ Worker: workers.Worker });
});
afterEach(() => {
  restore();
  jest.useRealTimers();
});

test("a peer socket authorization refusal preserves the sign-in cause during readiness validation", async () => {
  const server = { id: "peer", name: "Private", url: "https://peer.example" };
  const secureTarget = { ...target, origin: server.url, tls: true };
  const keepalive = new IdleKeepalive(secureTarget, {
    server,
    kind: "grant",
    token: "a".repeat(43),
    expiresAt: Date.now() + 60_000,
  });
  const pending = keepalive.verifyReady();
  const worker = workers.last();
  worker.emit({ type: "auth-required" });
  await expect(pending).rejects.toBeInstanceOf(ServerAuthenticationRequired);
  expect(worker.terminated).toBe(1);
  keepalive.stop();
});

// The older wait settles itself, but the slot it settles from belongs to the newer one: clearing it drops the ready.
test("a superseded readiness wait does not silence the newer one", async () => {
  const keepalive = new IdleKeepalive(target, credentials);
  const abort = new AbortController();
  const superseded = keepalive.verifyReady(abort.signal);
  const current = keepalive.verifyReady();

  abort.abort();
  await expect(superseded).rejects.toThrow(/aborted/);

  workers.last().emit({ type: "ready" });
  await current;
  keepalive.stop();
});

test("an old idle worker cannot invalidate or feed a restarted monitor", () => {
  const events: IdleEvent[] = [];
  const keepalive = new IdleKeepalive(target, credentials);
  keepalive.onEvent = (event) => events.push(event);
  keepalive.start();
  const old = workers.last();
  keepalive.stop();
  keepalive.start();
  old.emit({ type: "stall", detail: "late close" });
  old.emit({
    type: "samples",
    samples: [{ rtt: 12, timedOut: false, observedAtEpochMs: 1_000 }],
  });
  expect(events).toEqual([]);
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 8, timedOut: false, observedAtEpochMs: 1_100 }],
  });
  expect(
    events.some(
      (event) => event.type === "connectivity" && event.state === "connected",
    ),
  ).toBe(true);
  keepalive.stop();
});

test("idle latency buckets use each worker observation time", () => {
  const events: IdleEvent[] = [];
  const keepalive = new IdleKeepalive(target, credentials, 10_000);
  keepalive.onEvent = (event) => events.push(event);

  keepalive.start();
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 12, timedOut: false, observedAtEpochMs: 11_250 }],
  });
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 0, timedOut: true, observedAtEpochMs: 12_500 }],
  });

  const samples = events.flatMap((event) =>
    event.type === "latency" ? [event.sample] : [],
  );
  expect(samples.map((sample) => sample.endT)).toEqual([1_250, 2_500]);
  expect(samples.map((sample) => sample.t)).toEqual([1_250, 2_500]);
  keepalive.stop();
});

test("timeout-only keepalive batches do not recover offline connectivity", () => {
  const states: string[] = [];
  const keepalive = new IdleKeepalive(target, credentials);
  keepalive.onEvent = (event) => {
    if (event.type === "connectivity") states.push(event.state);
  };

  keepalive.start();
  workers.last().emit({ type: "stall", detail: "server stopped answering" });
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 0, timedOut: true, observedAtEpochMs: 1_000 }],
  });
  expect(states).toEqual(["offline"]);
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 8, timedOut: false, observedAtEpochMs: 1_100 }],
  });
  expect(states).toEqual(["offline", "connected"]);
  keepalive.stop();
});

test("adoption replays a provisional stall but does not infer offline from readiness alone", () => {
  const idle = new IdleKeepalive(target, credentials);
  idle.start();
  workers.last().emit({ type: "ready" });
  const events: IdleEvent[] = [];
  idle.onEvent = (event) => events.push(event);
  expect(events).toEqual([]);
  idle.onEvent = () => {};
  workers.last().emit({ type: "stall", detail: "closed" });
  idle.onEvent = (event) => events.push(event);
  expect(events).toEqual([{ type: "connectivity", state: "offline" }]);
  idle.stop();
});

test("adopting a verified idle monitor replays its proven connectivity without replaying RTTs", () => {
  const keepalive = new IdleKeepalive(target, credentials);
  keepalive.start();
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 8, timedOut: false, observedAtEpochMs: 1_000 }],
  });
  const events: IdleEvent[] = [];
  keepalive.onEvent = (event) => events.push(event);
  expect(events).toEqual([{ type: "connectivity", state: "connected" }]);
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 9, timedOut: false, observedAtEpochMs: 2_000 }],
  });
  expect(events.filter((event) => event.type === "connectivity")).toHaveLength(
    1,
  );
  expect(events.filter((event) => event.type === "latency")).toHaveLength(1);
  keepalive.stop();
});

test("stage latency preserves distinct times from one worker batch", () => {
  const observations: number[] = [];
  const channel = new LatencyChannel({
    host: host({ latency: (sample) => observations.push(sample.observedAtMs) }),
    target,
    credentials,
    timeOriginMs: 10_000,
  });

  channel.prime("reply-driven", true);
  channel.measure();
  workers.last().emit({
    type: "samples",
    samples: [
      { rtt: 8, timedOut: false, observedAtEpochMs: 10_100 },
      { rtt: 9, timedOut: false, observedAtEpochMs: 10_350 },
    ],
  });

  expect(observations).toEqual([100, 350]);
  channel.teardown();
});

test("a stage latency socket reopening does not itself resume recovery", () => {
  let resumes = 0;
  const channel = new LatencyChannel({
    host: host({ resumeLatency: () => resumes++ }),
    target,
    credentials,
  });

  channel.prime("medium", true);
  channel.measure();
  workers.last().emit({ type: "resume" });

  expect(resumes).toBe(0);
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 8, timedOut: false, observedAtEpochMs: 1_000 }],
  });
  expect(resumes).toBe(1);
  channel.teardown();
});

test("a stage timeout reaches the population as a timeout and does not resume recovery", () => {
  const outcomes: boolean[] = [];
  let resumes = 0;
  const channel = new LatencyChannel({
    host: host({
      latency: (sample) => outcomes.push(sample.timedOut),
      resumeLatency: () => resumes++,
    }),
    target,
    credentials,
  });
  channel.prime("medium", true);
  channel.measure();
  workers.last().emit({
    type: "samples",
    samples: [{ rtt: 250, timedOut: true, observedAtEpochMs: 1_000 }],
  });
  expect(outcomes).toEqual([true]);
  expect(resumes).toBe(0);
  channel.teardown();
});

test("path preparation collects only replies, never timeouts", async () => {
  const keepalive = new IdleKeepalive(target, credentials, 0);
  const collecting = keepalive.collectRtts();
  const reply = (rtt: number, timedOut = false) => ({
    rtt,
    timedOut,
    observedAtEpochMs: 1,
  });
  workers.last().emit({
    type: "samples",
    samples: [reply(9_999, true), ...[1, 2, 3, 4, 5].map((rtt) => reply(rtt))],
  });
  expect(await collecting).toEqual([1, 2, 3, 4, 5]);
  keepalive.stop();
});

test("a matched-probe ready event cancels the warmup establishment deadline", () => {
  jest.useFakeTimers();
  const failures: string[] = [];
  const channel = new LatencyChannel({
    host: host({ stallLatency: (detail) => failures.push(detail) }),
    target,
    credentials,
  });

  channel.prime("medium", true);
  expect(channel.ready).toBe(false);
  workers.last().emit({ type: "open" });
  expect(channel.ready).toBe(false);
  workers.last().emit({ type: "ready" });
  expect(channel.ready).toBe(true);
  jest.advanceTimersByTime(60_000);

  expect(failures).toEqual([]);
  channel.teardown();
  expect(channel.ready).toBe(false);
});

function finalizingChannel() {
  const observations: Parameters<ParticipantHost["latency"]>[0][] = [];
  const interruptions: { count: number; reason: string }[] = [];
  const stalls: string[] = [];
  let accountingComplete = true;
  const channel = new LatencyChannel({
    host: host({
      latency: (sample) => observations.push(sample),
      latencyIncomplete: () => (accountingComplete = false),
      latencyInterrupted: (count, reason) =>
        interruptions.push({ count, reason }),
      stallLatency: (detail) => stalls.push(detail),
    }),
    target,
    credentials,
  });
  channel.prime("medium", true);
  channel.measure();
  return {
    channel,
    worker: workers.last(),
    observations,
    interruptions,
    stalls,
    accountingComplete: () => accountingComplete,
  };
}

test("stage finalization keeps terminal outcomes until ack and excludes post-load RTTs", async () => {
  const { channel, worker, observations } = finalizingChannel();
  let finished = false;
  const ending = channel.finish().then(() => {
    finished = true;
  });
  const stop = worker.sent.at(-1) as { type: string; cutoffEpochMs: number };
  expect(stop.type).toBe("stop");
  await Promise.resolve();
  expect(finished).toBe(false);
  expect(worker.terminated).toBe(0);
  worker.emit({
    type: "samples",
    samples: [
      {
        rtt: 10,
        timedOut: false,
        reflectorHandlingMs: 2,
        sentAtEpochMs: stop.cutoffEpochMs - 20,
        observedAtEpochMs: stop.cutoffEpochMs - 10,
      },
      {
        rtt: 30,
        timedOut: false,
        reflectorHandlingMs: 9,
        sentAtEpochMs: stop.cutoffEpochMs - 10,
        observedAtEpochMs: stop.cutoffEpochMs + 20,
      },
      {
        rtt: 10,
        timedOut: false,
        sentAtEpochMs: stop.cutoffEpochMs + 1,
        observedAtEpochMs: stop.cutoffEpochMs + 11,
      },
    ],
  });
  expect(observations.map((sample) => sample.rttMs)).toEqual([10, 30]);
  expect(observations.map((sample) => sample.reflectorHandlingMs)).toEqual([
    2, 9,
  ]);
  expect(observations.map((sample) => sample.rttEligible)).toEqual([
    true,
    false,
  ]);
  worker.emit({ type: "stopped" });
  await ending;
  expect(finished).toBe(true);
  expect(worker.terminated).toBe(1);
});

test("terminal interruption counts use the same submission cutoff as reply outcomes", async () => {
  const { channel, worker, interruptions } = finalizingChannel();
  const ending = channel.finish();
  const { cutoffEpochMs } = worker.sent.at(-1) as {
    type: string;
    cutoffEpochMs: number;
  };
  worker.emit({
    type: "interrupted",
    sentAtEpochMs: [cutoffEpochMs - 1, cutoffEpochMs + 1],
    reason: "unresolved",
  });
  worker.emit({
    type: "interrupted",
    sentAtEpochMs: [cutoffEpochMs],
    reason: "send-failed",
  });
  expect(interruptions).toEqual([
    { count: 1, reason: "unresolved" },
    { count: 1, reason: "send-failed" },
  ]);
  worker.emit({ type: "stopped" });
  await ending;
});

test("abort settles a drain and keeps its late messages from the next stage", async () => {
  const { channel, worker, observations, interruptions } = finalizingChannel();
  const ending = channel.finish();
  channel.teardown();
  await ending;
  channel.prime("medium", true);
  worker.emit({
    type: "samples",
    samples: [{ rtt: 10, timedOut: false, observedAtEpochMs: 100 }],
  });
  worker.emit({
    type: "interrupted",
    sentAtEpochMs: [100],
    reason: "unresolved",
  });
  worker.emit({ type: "stopped" });
  expect(observations).toEqual([]);
  expect(interruptions).toEqual([]);
  expect(workers.last().terminated).toBe(0);
  channel.teardown();
});

test("worker failure settles a drain without manufacturing probe outcomes", async () => {
  const {
    channel,
    worker,
    observations,
    interruptions,
    stalls,
    accountingComplete,
  } = finalizingChannel();
  const ending = channel.finish();
  worker.onerror?.({ message: "worker crashed" } as ErrorEvent);
  await ending;
  expect(worker.terminated).toBe(1);
  expect(observations).toEqual([]);
  expect(interruptions).toEqual([]);
  expect(stalls).toEqual(["worker crashed"]);
  expect(accountingComplete()).toBe(false);
});

test("an unresponsive worker cannot hold stage finalization past the acknowledgement deadline", async () => {
  jest.useFakeTimers();
  const { channel, worker, observations, stalls, accountingComplete } =
    finalizingChannel();
  const ending = channel.finish();
  jest.advanceTimersByTime(PING_TIMEOUT_CEIL_MS + PING_STOP_MARGIN_MS - 1);
  expect(worker.terminated).toBe(0);
  jest.advanceTimersByTime(1);
  await ending;
  expect(worker.terminated).toBe(1);
  expect(observations).toEqual([]);
  expect(stalls).toEqual(["latency worker did not finish its pending probes"]);
  expect(accountingComplete()).toBe(false);
});

test("discarding an active stage marks its accounting unknown and terminates its worker", () => {
  const { channel, worker, accountingComplete } = finalizingChannel();
  channel.discard();
  expect(accountingComplete()).toBe(false);
  expect(worker.terminated).toBe(1);
});

test("discarding an already drained stage does not invent missing outcomes", async () => {
  const { channel, worker, accountingComplete } = finalizingChannel();
  const ending = channel.finish();
  worker.emit({ type: "stopped" });
  await ending;
  channel.discard();
  expect(accountingComplete()).toBe(true);
});
