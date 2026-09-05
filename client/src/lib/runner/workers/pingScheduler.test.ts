import { expect, test } from "bun:test";
import { encodePing } from "../real/wire";
import { createPingScheduler, type PingSchedulerClock } from "./pingScheduler";
import { testClock } from "./test-helpers.test";

class FakeWebSocket {
  sent: { at: number; frame: string }[] = [];
  constructor(private readonly clock: PingSchedulerClock) {}
  send(frame: string): void {
    this.sent.push({ at: this.clock.now(), frame });
  }
}

function makeHarness(
  pacing: Parameters<typeof createPingScheduler>[0],
  maxInFlight: number,
) {
  const clock = testClock() satisfies PingSchedulerClock;
  const socket = new FakeWebSocket(clock);
  const pending = new Set<number>();
  let id = 0;
  const scheduler = createPingScheduler(
    pacing,
    () => {
      if (pending.size >= maxInFlight) return false;
      pending.add(id);
      socket.send(encodePing(id++));
      return true;
    },
    clock,
  );
  return {
    clock,
    socket,
    scheduler,
    pending,
    pong() {
      const first = pending.values().next().value;
      if (first !== undefined) pending.delete(first);
      scheduler.complete();
    },
  };
}

function harness(intervalMs: number, maxInFlight = 16) {
  return makeHarness({ kind: "fixed", intervalMs }, maxInFlight);
}

function replyHarness(backupDelayMs: () => number, maxInFlight = 4) {
  const h = makeHarness({ kind: "reply-driven", backupDelayMs }, maxInFlight);
  return {
    ...h,
    pong(id: number) {
      if (h.pending.delete(id)) h.scheduler.complete();
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
    expect(h.socket.sent.every(({ frame }) => frame.startsWith("PING,"))).toBe(
      true,
    );
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
