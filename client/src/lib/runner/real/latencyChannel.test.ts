// The idle keepalive's readiness wait. Its slot is shared: validateConnections
// aborts a probe and starts the next one without awaiting it, so two waits can
// exist over one worker.
import { test, expect } from "bun:test";
import { IdleKeepalive, LatencyChannel } from "./latencyChannel";
import type { CoreHost } from "../core";
import type { LatencyTarget } from "../../api/endpoints";

class FakeWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  static last: FakeWorker | null = null;

  constructor() {
    FakeWorker.last = this;
  }
  postMessage(): void {}
  terminate(): void {}
  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

const target: LatencyTarget = {
  id: "http://meter.test:7246",
  origin: "http://meter.test:7246",
  transport: "websocket",
  protocol: "http1",
  tls: false,
  routes: { probe: "/probe", ping: "/ws/ping" },
};

// The older wait settles itself, but the slot it settles from belongs to the
// newer one: clearing it drops the ready message the channel is about to send,
// and the newer wait then times out on a bus that is already up.
test("a superseded readiness wait does not silence the newer one", async () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
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

    FakeWorker.last!.emit({ type: "ready" });
    for (let turn = 0; turn < 10 && !ready; turn++) await Promise.resolve();
    expect(ready).toBe(true);
    await current;
    keepalive.stop();
  } finally {
    globalThis.Worker = realWorker;
  }
});

test("idle latency buckets use each worker observation time", () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
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
    FakeWorker.last!.emit({
      type: "samples",
      samples: [{ rtt: 12, lost: false, observedAtEpochMs: 11_250 }],
    });
    FakeWorker.last!.emit({
      type: "samples",
      samples: [{ rtt: 0, lost: true, observedAtEpochMs: 12_500 }],
    });

    const samples = events.flatMap((event) =>
      event.type === "latency" ? [event.sample] : [],
    );
    expect(samples.map((sample) => sample.endT)).toEqual([1_250, 2_500]);
    expect(samples.map((sample) => sample.t)).toEqual([1_250, 2_500]);
    keepalive.stop();
  } finally {
    globalThis.Worker = realWorker;
  }
});

test("stage latency preserves distinct times from one worker batch", () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
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
    channel.measure(false);
    FakeWorker.last!.emit({
      type: "samples",
      samples: [
        { rtt: 8, lost: false, observedAtEpochMs: 10_100 },
        { rtt: 9, lost: false, observedAtEpochMs: 10_350 },
      ],
    });

    expect(observations).toEqual([100, 350]);
    channel.teardown();
  } finally {
    globalThis.Worker = realWorker;
  }
});

test("a stage latency socket reopening does not itself resume recovery", () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
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
    channel.measure(false);
    FakeWorker.last!.emit({ type: "resume" });

    expect(resumes).toBe(0);
    FakeWorker.last!.emit({
      type: "samples",
      samples: [{ rtt: 8, lost: false, observedAtEpochMs: 1_000 }],
    });
    expect(resumes).toBe(1);
    channel.teardown();
  } finally {
    globalThis.Worker = realWorker;
  }
});

test("READY cancels the stage channel's warmup establishment deadline", () => {
  const realWorker = globalThis.Worker;
  const realSetTimeout = globalThis.setTimeout;
  const realClearTimeout = globalThis.clearTimeout;
  let deadline: (() => void) | null = null;
  let deadlineActive = false;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  globalThis.setTimeout = ((handler: TimerHandler) => {
    deadline = handler as () => void;
    deadlineActive = true;
    return 1 as unknown as ReturnType<typeof setTimeout>;
  }) as unknown as typeof setTimeout;
  globalThis.clearTimeout = (() => {
    deadlineActive = false;
  }) as typeof clearTimeout;
  try {
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
    FakeWorker.last!.emit({ type: "ready" });
    if (deadlineActive) (deadline as (() => void) | null)?.();

    expect(failures).toEqual([]);
    channel.teardown();
  } finally {
    globalThis.Worker = realWorker;
    globalThis.setTimeout = realSetTimeout;
    globalThis.clearTimeout = realClearTimeout;
  }
});
