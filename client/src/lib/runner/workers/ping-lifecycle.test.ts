import { stubGlobals } from "../../test-helpers.test";
import { expect, test } from "bun:test";
import { LatencyAccumulator } from "../latencySummary";
import type { PingSample } from "./pingSample";

type Output =
  | { type: "samples"; samples: PingSample[] }
  | { type: "interrupted"; sentAtEpochMs: number[]; reason: string }
  | {
      type: "open" | "ready" | "stopped" | "stall" | "resume" | "auth-required";
    };

let realm = 0;
async function withWorker(
  run: (worker: {
    posted: Output[];
    advance: (ms: number) => void;
    jump: (ms: number) => void;
    reply: (id: number) => void;
    send: (message: unknown) => void;
    stop: (cutoff?: number) => void;
    socket: FakeSocket;
    now: () => number;
    samples: () => PingSample[];
  }) => void | Promise<void>,
) {
  let now = 0;
  let nextTimer = 0;
  const timers = new Map<
    number,
    { at: number; interval?: number; callback: () => void }
  >();
  const posted: Output[] = [];
  let socket!: FakeSocket;
  const timeout = (callback: () => void, delay = 0, interval?: number) => {
    const id = ++nextTimer;
    timers.set(id, { at: now + delay, interval, callback });
    return id;
  };
  const overrides = {
    self: globalThis,
    WebSocket: class extends FakeSocket {
      constructor() {
        super();
        socket = this;
      }
    },
    performance: { now: () => now, timeOrigin: 10_000 },
    postMessage: (message: Output) => posted.push(message),
    setTimeout: timeout,
    clearTimeout: (id: number) => timers.delete(id),
    setInterval: (callback: () => void, delay: number) =>
      timeout(callback, delay, delay),
    clearInterval: (id: number) => timers.delete(id),
    onmessage: null,
  };
  const restore = stubGlobals(overrides);
  try {
    await import(`./ping-worker.ts?lifecycle=${realm++}`);
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
    socket.onopen();
    send({ type: "measure" });
    // Finish the warmup request, starting the first eligible probe at time zero.
    socket.onmessage({ data: "PONG,0;TIME,0" });
    await run({
      posted,
      socket,
      send,
      now: () => now,
      jump: (ms) => {
        now += ms;
      },
      advance(ms) {
        const end = now + ms;
        for (;;) {
          const next = [...timers.entries()]
            .filter(([, timer]) => timer.at <= end)
            .sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
          if (!next) break;
          const [id, timer] = next;
          now = Math.max(now, timer.at);
          if (timer.interval) timer.at = now + timer.interval;
          else timers.delete(id);
          timer.callback();
        }
        now = end;
      },
      reply: (id) => socket.onmessage({ data: `PONG,${id};TIME,0` }),
      stop: (cutoff = now) =>
        send({ type: "stop", cutoffEpochMs: 10_000 + cutoff }),
      samples: () =>
        posted.flatMap((message) =>
          message.type === "samples" ? message.samples : [],
        ),
    });
  } finally {
    restore();
  }
}

class FakeSocket {
  static OPEN = 1;
  readyState = 1;
  closed = 0;
  failSends = false;
  rejectSends = false;
  parkSends = false;
  pings: number[] = [];
  onopen = () => {};
  onmessage = (_event: { data: string }) => {};
  onclose = (_event: { code: number; reason: string }) => {};
  send(message: string): void | Promise<void> {
    if (this.failSends) throw new Error("local send failed");
    if (this.rejectSends)
      return Promise.reject(new Error("local datagram write rejected"));
    if (this.parkSends) return new Promise(() => {});
    if (message.startsWith("PING,")) this.pings.push(Number(message.slice(5)));
  }
  close(): void {
    this.closed++;
  }
  disconnect(): void {
    this.readyState = 3;
    this.onclose({ code: 1006, reason: "" });
  }
}

test("stop flushes an unbatched reply, drains a pending reply, then acknowledges without another send", async () => {
  await withWorker(({ advance, reply, stop, posted, socket, samples }) => {
    advance(1);
    reply(1);
    expect(samples()).toHaveLength(0);
    stop();
    expect(samples()).toHaveLength(1);
    const sent = socket.pings.length;
    expect(posted.at(-1)?.type).not.toBe("stopped");
    advance(20);
    reply(2);
    expect(samples()).toHaveLength(2);
    expect(posted.at(-1)?.type).toBe("stopped");
    expect(socket.closed).toBe(1);
    advance(20_000);
    expect(socket.pings).toHaveLength(sent);
    expect(samples().every((sample) => !sample.lost)).toBe(true);
  });
});

test("drain retains original probe deadlines and emits timeout before its acknowledgement", async () => {
  await withWorker(({ advance, stop, posted, samples }) => {
    stop();
    advance(249);
    expect(samples()).toHaveLength(0);
    expect(posted.at(-1)?.type).not.toBe("stopped");
    advance(1);
    expect(samples()).toEqual([
      {
        rtt: 250,
        lost: true,
        observedAtEpochMs: 10_250,
        sentAtEpochMs: 10_000,
      },
    ]);
    expect(posted.at(-1)?.type).toBe("stopped");
  });
});

test("a reply past its original deadline is a timeout even before the sweeper runs", async () => {
  await withWorker(({ jump, reply, stop, samples }) => {
    jump(251);
    reply(1);
    stop(0);
    expect(samples()).toEqual([
      {
        rtt: 250,
        lost: true,
        observedAtEpochMs: 10_250,
        sentAtEpochMs: 10_000,
      },
    ]);
  });
});

test("disconnect during drain marks unresolved probes without timeout evidence or reconnect", async () => {
  await withWorker(({ stop, socket, posted, samples, advance }) => {
    stop();
    socket.disconnect();
    expect(samples()).toHaveLength(0);
    expect(posted).toContainEqual({
      type: "interrupted",
      sentAtEpochMs: [10_000],
      reason: "unresolved",
    });
    expect(posted.at(-1)?.type).toBe("stopped");
    advance(20_000);
    expect(socket.pings).toEqual([0, 1]);
  });
});

test("a local send failure is excluded from the probe timeout denominator", async () => {
  await withWorker(({ reply, stop, socket, posted, samples, advance }) => {
    socket.failSends = true;
    reply(1);
    stop();
    advance(20_000);
    expect(samples()).toHaveLength(1);
    expect(samples()[0].lost).toBe(false);
    expect(posted).toContainEqual({
      type: "interrupted",
      sentAtEpochMs: [10_000],
      reason: "send-failed",
    });
    expect(posted.at(-1)?.type).toBe("stopped");
  });
});

test("a delayed stop excludes probes submitted after its cutoff", async () => {
  await withWorker(({ advance, reply, stop, posted, samples }) => {
    advance(1);
    reply(1);
    advance(1);
    reply(2);
    stop(0);
    expect(samples()).toHaveLength(1);
    expect(samples()[0].sentAtEpochMs).toBe(10_000);
    expect(posted.at(-1)?.type).toBe("stopped");
  });
});

test("a rejected asynchronous write is a local send failure, not a timeout", async () => {
  await withWorker(async ({ reply, stop, socket, posted, samples }) => {
    socket.rejectSends = true;
    reply(1);
    stop();
    await Promise.resolve();
    expect(samples()).toHaveLength(1);
    expect(posted).toContainEqual({
      type: "interrupted",
      sentAtEpochMs: [10_000],
      reason: "send-failed",
    });
    expect(posted.at(-1)?.type).toBe("stopped");
  });
});

test("an unconfirmed write remains unresolved when its bounded drain ends", async () => {
  await withWorker(({ reply, stop, socket, posted, samples, advance }) => {
    socket.parkSends = true;
    reply(1);
    stop();
    advance(250);
    expect(samples()).toHaveLength(1);
    expect(samples()[0].lost).toBe(false);
    expect(posted).toContainEqual({
      type: "interrupted",
      sentAtEpochMs: [10_000],
      reason: "unresolved",
    });
    expect(posted.at(-1)?.type).toBe("stopped");
  });
});

test("disconnect preserves a deadline outcome that preceded the connection gap", async () => {
  await withWorker(({ jump, socket, samples, stop }) => {
    jump(251);
    socket.disconnect();
    stop();
    expect(samples()).toHaveLength(1);
    expect(samples()[0].lost).toBe(true);
  });
});

for (const failure of ["failSends", "rejectSends"] as const) {
  test(`${failure} preserves buffered reply order across the jitter interruption`, async () => {
    await withWorker(async ({ advance, reply, stop, socket, posted }) => {
      advance(1);
      reply(1);
      advance(2);
      socket[failure] = true;
      reply(2);
      await Promise.resolve();
      stop();
      const outcomes = posted.filter(
        (message) =>
          message.type === "samples" || message.type === "interrupted",
      );
      expect(outcomes.map((message) => message.type)).toEqual([
        "samples",
        "interrupted",
      ]);
      const accumulator = new LatencyAccumulator();
      for (const outcome of outcomes) {
        if (outcome.type === "samples") {
          for (const sample of outcome.samples)
            accumulator.observe(sample.rtt, sample.lost, 0);
        } else if (outcome.type === "interrupted")
          accumulator.interrupt(outcome.sentAtEpochMs.length, "send-failed");
      }
      // A later reply must not form an RTT variation pair across the failed send.
      accumulator.observe(99, false, 0);
      expect(accumulator.snapshot()).toMatchObject({
        probeCount: 3,
        sendFailureCount: 1,
        jitterPairs: 1,
        jitterMs: 1,
      });
    });
  });
}
