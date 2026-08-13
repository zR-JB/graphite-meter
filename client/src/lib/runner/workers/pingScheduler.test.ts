import { expect, test } from "bun:test";
import { decode, encode } from "../real/wire";
import { PingScheduler, type PingSchedulerClock } from "./pingScheduler";

class ControlledClock implements PingSchedulerClock {
  time = 0;
  #nextId = 0;
  #timers = new Map<number, { at: number; callback: () => void }>();

  now(): number {
    return this.time;
  }

  setTimeout(callback: () => void, delayMs: number): number {
    const id = this.#nextId++;
    this.#timers.set(id, { at: this.time + delayMs, callback });
    return id;
  }

  clearTimeout(timer: unknown): void {
    this.#timers.delete(timer as number);
  }

  advance(ms: number): void {
    const end = this.time + ms;
    for (;;) {
      const next = [...this.#timers.entries()]
        .filter(([, timer]) => timer.at <= end)
        .sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
      if (!next) break;
      this.time = next[1].at;
      this.#timers.delete(next[0]);
      next[1].callback();
    }
    this.time = end;
  }
}

class FakeWebSocket {
  sent: { at: number; frame: string }[] = [];
  constructor(private readonly clock: ControlledClock) {}
  send(frame: string): void {
    this.sent.push({ at: this.clock.now(), frame });
  }
}

function harness(intervalMs: number, maxInFlight = 16) {
  const clock = new ControlledClock();
  const socket = new FakeWebSocket(clock);
  const pending = new Set<number>();
  let id = 0;
  let reports = 0;
  const scheduler = new PingScheduler(
    { kind: "fixed", intervalMs },
    () => {
      if (pending.size >= maxInFlight) return false;
      pending.add(id);
      socket.send(encode({ op: "PING", id: id++ }));
      return true;
    },
    clock,
  );
  return {
    clock,
    socket,
    scheduler,
    pending,
    pong(report = true) {
      const first = pending.values().next().value;
      if (first !== undefined) pending.delete(first);
      if (report) reports++;
      scheduler.complete();
    },
    reportCount: () => reports,
  };
}

function replyHarness(backupDelayMs: () => number, maxInFlight = 4) {
  const clock = new ControlledClock();
  const socket = new FakeWebSocket(clock);
  const pending = new Set<number>();
  let id = 0;
  const scheduler = new PingScheduler(
    { kind: "reply-driven", backupDelayMs },
    () => {
      if (pending.size >= maxInFlight) return false;
      pending.add(id);
      socket.send(encode({ op: "PING", id: id++ }));
      return true;
    },
    clock,
  );
  return {
    clock,
    socket,
    scheduler,
    pending,
    pong(id: number) {
      if (pending.delete(id)) scheduler.complete();
    },
  };
}

for (const [cadence, expected] of [
  [80, 51],
  [250, 17],
  [600, 7],
] as const) {
  test(`${cadence} ms cadence controls application PING count`, () => {
    const h = harness(cadence);
    h.scheduler.start();
    for (let elapsed = 0; elapsed < 4_000; elapsed++) {
      h.clock.advance(1);
      h.pong(); // immediate/sub-millisecond RTT cannot pull the next send early
    }
    expect(h.socket.sent).toHaveLength(expected);
    expect(
      h.socket.sent.every(({ frame }) => decode(frame).op === "PING"),
    ).toBe(true);
    for (let i = 1; i < h.socket.sent.length; i++)
      expect(
        h.socket.sent[i].at - h.socket.sent[i - 1].at,
      ).toBeGreaterThanOrEqual(cadence);
  });
}

test("RTT longer than cadence resumes overdue without a catch-up burst", () => {
  const h = harness(80, 1);
  h.scheduler.start();
  h.clock.advance(125);
  h.pong();
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 125]);
  h.pong();
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 125]);
  h.clock.advance(80);
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 125, 205]);
});

test("measurement/report downsampling does not change wire sends", () => {
  const full = harness(80);
  const sparse = harness(80);
  full.scheduler.start();
  sparse.scheduler.start();
  for (let elapsed = 0; elapsed < 1_000; elapsed++) {
    full.clock.advance(1);
    sparse.clock.advance(1);
    full.pong();
    sparse.pong(elapsed % 250 === 0);
  }
  expect(sparse.socket.sent.length).toBe(full.socket.sent.length);
  expect(sparse.reportCount()).toBeLessThan(full.reportCount());
});

test("stop and reset leave no live send timer", () => {
  const h = harness(80);
  h.scheduler.start();
  h.scheduler.stop();
  h.clock.advance(1_000);
  expect(h.socket.sent).toHaveLength(1);
  h.scheduler.reset();
  h.scheduler.start();
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 1000]);
});

test("a measurement boundary restarts fixed cadence with an immediate send", () => {
  const h = harness(600);
  h.scheduler.start();
  h.clock.advance(590);

  h.scheduler.restartNow();
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 590]);
  h.clock.advance(599);
  expect(h.socket.sent).toHaveLength(2);
  h.clock.advance(1);
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 590, 1190]);
});

test("cadence can settle after a fast probe without an early send", () => {
  const h = harness(120);
  h.scheduler.start();
  h.clock.advance(120);
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 120]);

  h.scheduler.setInterval(1000);
  h.clock.advance(999);
  expect(h.socket.sent).toHaveLength(2);
  h.clock.advance(1);
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 120, 1120]);
});

test("reply-driven cadence continues immediately on each pong", () => {
  const h = replyHarness(() => 250);
  h.scheduler.start();
  h.clock.advance(2);
  h.pong(0);
  h.clock.advance(1);
  h.pong(1);
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 2, 3]);
});

test("reply-driven backup follows the live RTT heuristic and respects its cap", () => {
  let backupMs = 100;
  const h = replyHarness(() => backupMs, 3);
  h.scheduler.start();
  backupMs = 20;
  h.clock.advance(100);
  h.clock.advance(40);
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 100, 120]);
  h.clock.advance(1_000);
  expect(h.socket.sent).toHaveLength(3);

  h.pong(1);
  expect(h.socket.sent.map((send) => send.at)).toEqual([0, 100, 120, 1140]);
});
