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
  const keepalive = new IdleKeepalive({
    host: () => ({ emit() {} }) as unknown as CoreHost,
    throughputTarget: () => null,
    latencyTarget: () => target,
  });
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
  const keepalive = new IdleKeepalive({
    host: () =>
      ({
        emit(event: Parameters<CoreHost["emit"]>[0]) {
          events.push(event);
        },
      }) as unknown as CoreHost,
    throughputTarget: () => null,
    latencyTarget: () => target,
    timeOriginMs: 10_000,
  });

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
  const keepalive = new IdleKeepalive({
    host: () =>
      ({
        emit(event: Parameters<CoreHost["emit"]>[0]) {
          if (event.type === "connectivity") states.push(event.state);
        },
      }) as unknown as CoreHost,
    throughputTarget: () => null,
    latencyTarget: () => target,
  });

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
