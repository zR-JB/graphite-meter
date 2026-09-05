// Its slot is shared: validateConnections aborts a probe and starts the next one without awaiting it, so two waits.
import { test, expect, afterEach, beforeEach } from "bun:test";
import { IdleKeepalive, LatencyChannel } from "./latencyChannel";
import type { CoreHost } from "../core";
import type { LatencyTarget } from "../../api/endpoints";
import { TestWorker } from "./test-helpers.test";

const target: LatencyTarget = {
  id: "http://meter.test:7246",
  origin: "http://meter.test:7246",
  transport: "websocket",
  protocol: "http1",
  tls: false,
  routes: { probe: "/probe", ping: "/ws/ping" },
};

const realWorker = globalThis.Worker;
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;
afterEach(() => {
  globalThis.Worker = realWorker;
  globalThis.setTimeout = realSetTimeout;
  globalThis.clearTimeout = realClearTimeout;
});
beforeEach(() => {
  globalThis.Worker = TestWorker as unknown as typeof Worker;
});

// The older wait settles itself, but the slot it settles from belongs to the newer one: clearing it drops the ready.
test("a superseded readiness wait does not silence the newer one", async () => {
  const keepalive = new IdleKeepalive(target);
  const abort = new AbortController();
  const superseded = keepalive.verifyReady(abort.signal);
  let ready = false;
  const current = keepalive.verifyReady().then(() => (ready = true));

  abort.abort();
  await expect(superseded).rejects.toThrow(/aborted/);

  TestWorker.last!.emit({ type: "ready" });
  for (let turn = 0; turn < 10 && !ready; turn++) await Promise.resolve();
  expect(ready).toBe(true);
  await current;
  keepalive.stop();
});

test("idle latency buckets use each worker observation time", () => {
  const events: Parameters<CoreHost["emit"]>[0][] = [];
  const keepalive = new IdleKeepalive(target, 10_000);
  keepalive.onEvent = (event) => events.push(event);

  keepalive.start();
  TestWorker.last!.emit({
    type: "samples",
    samples: [{ rtt: 12, lost: false, observedAtEpochMs: 11_250 }],
  });
  TestWorker.last!.emit({
    type: "samples",
    samples: [{ rtt: 0, lost: true, observedAtEpochMs: 12_500 }],
  });

  const samples = events.flatMap((event) =>
    event.type === "latency" ? [event.sample] : [],
  );
  expect(samples.map((sample) => sample.endT)).toEqual([1_250, 2_500]);
  expect(samples.map((sample) => sample.t)).toEqual([1_250, 2_500]);
  keepalive.stop();
});

test("loss-only keepalive batches do not recover offline connectivity", () => {
  const states: string[] = [];
  const keepalive = new IdleKeepalive(target);
  keepalive.onEvent = (event) => {
    if (event.type === "connectivity") states.push(event.state);
  };

  keepalive.start();
  TestWorker.last!.emit({ type: "stall", detail: "server stopped answering" });
  TestWorker.last!.emit({
    type: "samples",
    samples: [{ rtt: 0, lost: true, observedAtEpochMs: 1_000 }],
  });
  expect(states).toEqual(["offline"]);
  TestWorker.last!.emit({
    type: "samples",
    samples: [{ rtt: 8, lost: false, observedAtEpochMs: 1_100 }],
  });
  expect(states).toEqual(["offline", "connected"]);
  keepalive.stop();
});

test("adoption replays a provisional stall but does not infer offline from readiness alone", () => {
  const idle = new IdleKeepalive(target);
  idle.start();
  TestWorker.last!.emit({ type: "ready" });
  const events: Parameters<CoreHost["emit"]>[0][] = [];
  idle.onEvent = (event) => events.push(event);
  expect(events).toEqual([]);
  idle.onEvent = () => {};
  TestWorker.last!.emit({ type: "stall", detail: "closed" });
  idle.onEvent = (event) => events.push(event);
  expect(events).toEqual([{ type: "connectivity", state: "offline" }]);
  idle.stop();
});

test("adopting a verified idle monitor replays its proven connectivity without replaying RTTs", () => {
  const keepalive = new IdleKeepalive(target);
  keepalive.start();
  TestWorker.last!.emit({
    type: "samples",
    samples: [{ rtt: 8, lost: false, observedAtEpochMs: 1_000 }],
  });
  const events: Parameters<CoreHost["emit"]>[0][] = [];
  keepalive.onEvent = (event) => events.push(event);
  expect(events).toEqual([{ type: "connectivity", state: "connected" }]);
  TestWorker.last!.emit({
    type: "samples",
    samples: [{ rtt: 9, lost: false, observedAtEpochMs: 2_000 }],
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
    host: () =>
      ({
        config: { pingCadence: "reply-driven", loadedPingCadence: "medium" },
        ingestLatency(observation: { observedAtMs: number }) {
          observations.push(observation.observedAtMs);
        },
      }) as unknown as CoreHost,
    target: () => target,
    stall() {},
    resume() {},
    timeOriginMs: 10_000,
  });

  channel.prime("websocket", true);
  channel.measure();
  TestWorker.last!.emit({
    type: "samples",
    samples: [
      { rtt: 8, lost: false, observedAtEpochMs: 10_100 },
      { rtt: 9, lost: false, observedAtEpochMs: 10_350 },
    ],
  });

  expect(observations).toEqual([100, 350]);
  channel.teardown();
});

test("a stage latency socket reopening does not itself resume recovery", () => {
  let resumes = 0;
  const channel = new LatencyChannel({
    host: () =>
      ({
        config: { pingCadence: "medium", loadedPingCadence: "medium" },
        ingestLatency() {},
      }) as unknown as CoreHost,
    target: () => target,
    stall() {},
    resume() {
      resumes++;
    },
  });

  channel.prime("websocket", true);
  channel.measure();
  TestWorker.last!.emit({ type: "resume" });

  expect(resumes).toBe(0);
  TestWorker.last!.emit({
    type: "samples",
    samples: [{ rtt: 8, lost: false, observedAtEpochMs: 1_000 }],
  });
  expect(resumes).toBe(1);
  channel.teardown();
});

test("READY cancels the stage channel's warmup establishment deadline", () => {
  let deadline: (() => void) | null = null;
  let deadlineActive = false;
  globalThis.setTimeout = ((handler: TimerHandler) => {
    deadline = handler as () => void;
    deadlineActive = true;
    return 1 as unknown as ReturnType<typeof setTimeout>;
  }) as unknown as typeof setTimeout;
  globalThis.clearTimeout = (() => {
    deadlineActive = false;
  }) as typeof clearTimeout;
  const failures: string[] = [];
  const channel = new LatencyChannel({
    host: () =>
      ({
        config: { pingCadence: "medium", loadedPingCadence: "medium" },
        failStage: (_stage: string, _reason: string, detail: string) =>
          failures.push(detail),
      }) as unknown as CoreHost,
    target: () => target,
    stall: (detail) => failures.push(detail),
    resume() {},
  });

  channel.prime("websocket", true);
  TestWorker.last!.emit({ type: "ready" });
  if (deadlineActive) (deadline as (() => void) | null)?.();

  expect(failures).toEqual([]);
  channel.teardown();
});

function finalizingChannel() {
  const observations: { rttMs: number; rttEligible?: boolean }[] = [];
  const interruptions: { count: number; reason: string }[] = [];
  const stalls: string[] = [];
  let accountingComplete = true;
  const channel = new LatencyChannel({
    host: () =>
      ({
        config: { pingCadence: "medium", loadedPingCadence: "medium" },
        ingestLatency(observation: { rttMs: number; rttEligible?: boolean }) {
          observations.push(observation);
        },
        ingestLatencyAccountingIncomplete() {
          accountingComplete = false;
        },
        ingestLatencyInterruption(count: number, reason: string) {
          interruptions.push({ count, reason });
        },
      }) as unknown as CoreHost,
    target: () => target,
    stall: (detail) => stalls.push(detail),
    resume() {},
  });
  channel.prime("websocket", true);
  channel.measure();
  return {
    channel,
    worker: TestWorker.last!,
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
        lost: false,
        sentAtEpochMs: stop.cutoffEpochMs - 20,
        observedAtEpochMs: stop.cutoffEpochMs - 10,
      },
      {
        rtt: 30,
        lost: false,
        sentAtEpochMs: stop.cutoffEpochMs - 10,
        observedAtEpochMs: stop.cutoffEpochMs + 20,
      },
      {
        rtt: 10,
        lost: false,
        sentAtEpochMs: stop.cutoffEpochMs + 1,
        observedAtEpochMs: stop.cutoffEpochMs + 11,
      },
    ],
  });
  expect(observations.map((sample) => sample.rttMs)).toEqual([10, 30]);
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
  const { cutoffEpochMs } = worker.sent.at(-1) as { cutoffEpochMs: number };
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

test("abort settles an in-flight drain and prevents its late worker messages reaching a replacement stage", async () => {
  const { channel, worker, observations, interruptions } = finalizingChannel();
  const ending = channel.finish();
  channel.teardown();
  await ending;
  channel.prime("websocket", true);
  worker.emit({
    type: "samples",
    samples: [{ rtt: 10, lost: false, observedAtEpochMs: 100 }],
  });
  worker.emit({
    type: "interrupted",
    sentAtEpochMs: [100],
    reason: "unresolved",
  });
  worker.emit({ type: "stopped" });
  expect(observations).toEqual([]);
  expect(interruptions).toEqual([]);
  expect(TestWorker.last!.terminated).toBe(0);
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
  let deadline!: () => void;
  let delay = 0;
  globalThis.setTimeout = ((handler: () => void, ms: number) => {
    deadline = handler;
    delay = ms;
    return 1;
  }) as unknown as typeof setTimeout;
  globalThis.clearTimeout = (() => {}) as typeof clearTimeout;
  const { channel, worker, observations, stalls, accountingComplete } =
    finalizingChannel();
  const ending = channel.finish();
  expect(delay).toBe(10_250);
  deadline();
  await ending;
  expect(worker.terminated).toBe(1);
  expect(observations).toEqual([]);
  expect(stalls).toEqual(["ping worker did not finish its pending probes"]);
  expect(accountingComplete()).toBe(false);
});

test("discarding an active stage marks unknown accounting before terminating its worker", () => {
  const { channel, worker, accountingComplete } = finalizingChannel();
  const terminate = worker.terminate.bind(worker);
  worker.terminate = () => {
    expect(accountingComplete()).toBe(false);
    terminate();
  };
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
