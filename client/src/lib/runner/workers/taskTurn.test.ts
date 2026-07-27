import { expect, test } from "bun:test";
import { taskTurn } from "./taskTurn";

test("a turn resolves past the microtask checkpoint that precedes it", async () => {
  const order: string[] = [];
  const turn = taskTurn().then(() => order.push("turn"));
  void Promise.resolve().then(() => order.push("microtask"));
  await turn;
  expect(order).toEqual(["microtask", "turn"]);
});

// The order is the point: a timer armed from a timer's own task is floored at
// 4ms, one armed from a port task is not. Reversing them halves the datagram
// loops' rate, and nothing in the type system says otherwise.
test("a turn arms its timer from a port task, which is what keeps it unclamped", async () => {
  const realTimeout = globalThis.setTimeout;
  const RealChannel = globalThis.MessageChannel;
  const order: string[] = [];
  globalThis.setTimeout = ((...args: Parameters<typeof setTimeout>) => {
    order.push("timer");
    return realTimeout(...args);
  }) as typeof setTimeout;
  globalThis.MessageChannel = class extends RealChannel {
    constructor() {
      super();
      order.push("port");
    }
  };
  try {
    await taskTurn();
  } finally {
    globalThis.setTimeout = realTimeout;
    globalThis.MessageChannel = RealChannel;
  }
  expect(order).toEqual(["port", "timer"]);
});
