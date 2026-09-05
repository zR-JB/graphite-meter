import { stubGlobals } from "../../test-helpers.test";
import { expect, test } from "bun:test";
import { RunAccumulator } from "../evaluation";
import { DEFAULT_CONFIG } from "../../state/defaults";
import type { PingSample } from "./pingSample";

type Batch = { type: "samples"; samples: PingSample[] };

async function replay(replies: number) {
  let now = 1_000;
  const intervals: (() => void)[] = [];
  const batches: Batch[] = [];
  let socket: FakeSocket;
  class FakeSocket {
    static OPEN = 1;
    readyState = 1;
    onopen = () => {};
    onmessage = (_event: { data: string }) => {};
    constructor() {
      socket = this;
    }
    send(_message: string) {}
  }
  const overrides = {
    self: globalThis,
    WebSocket: FakeSocket,
    performance: { now: () => now, timeOrigin: 50_000 },
    postMessage: (message: { type: string }) => {
      if (message.type === "samples") batches.push(message as Batch);
    },
    setInterval: (callback: () => void) => intervals.push(callback),
    setTimeout: () => 1,
    clearTimeout: () => {},
    onmessage: null,
  };
  const restore = stubGlobals(overrides);
  try {
    await import(`./ping-worker.ts?accounting=${replies}`);
    const handler = globalThis.onmessage as (event: MessageEvent) => void;
    const send = (data: unknown) => handler({ data } as MessageEvent);
    send({
      type: "start",
      url: "ws://meter.test/ping",
      transport: "websocket",
      intervalMs: 250,
      replyDriven: true,
      maxInFlight: 4,
      lossK: 4,
      lossFloorMs: 250,
    });
    socket!.onopen();
    send({ type: "measure" });
    // ID zero was sent during warmup; its reply must stay outside the measured population.
    for (let id = 0; id <= replies; id++) {
      now++;
      socket!.onmessage({ data: `PONG,${id};TIME,0` });
      socket!.onmessage({ data: `PONG,${id};TIME,0` });
    }
    // The last reply started one more probe; expire it, then deliver a late reply and an unknown ID.
    now += 2_000;
    intervals[0]();
    socket!.onmessage({ data: `PONG,${replies + 1};TIME,0` });
    socket!.onmessage({ data: "PONG,999999;TIME,0" });
    intervals[1]();
    return batches;
  } finally {
    restore();
  }
}

test("reply-driven accounting retains nine replies and one timeout regardless of display cadence", async () => {
  const samples = (await replay(9)).flatMap((batch) => batch.samples);
  expect(samples.filter((sample) => !sample.lost)).toHaveLength(9);
  expect(samples.filter((sample) => sample.lost)).toHaveLength(1);
  expect(samples[0].observedAtEpochMs).toBe(51_002);
  const accum = new RunAccumulator();
  for (const sample of samples)
    accum.pushLatency("latency", sample.rtt, sample.lost);
  expect(accum.latencyResult(DEFAULT_CONFIG)!.probeTimeoutPct).toBe(10);
});

test("a fast reply burst produces bounded batches without discarding outcomes", async () => {
  const batches = await replay(1_025);
  expect(batches.length).toBeGreaterThan(1);
  expect(batches.every((batch) => batch.samples.length <= 128)).toBe(true);
  const samples = batches.flatMap((batch) => batch.samples);
  expect(samples.filter((sample) => !sample.lost)).toHaveLength(1_025);
  expect(samples.filter((sample) => sample.lost)).toHaveLength(1);
});
