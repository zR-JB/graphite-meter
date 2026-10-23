import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { HandshakeLedger } from "./webtransport";

beforeEach(() => jest.useFakeTimers());
afterEach(() => jest.useRealTimers());

test("four failed handshakes within 5 minutes throttle WebTransport until the oldest of them is 5 minutes old", () => {
  const ledger = new HandshakeLedger();
  const states: boolean[] = [];
  for (const wait of [0, 100_000, 100_000, 110_000, 10_000]) {
    jest.advanceTimersByTime(wait);
    ledger.failed();
    states.push(ledger.throttled());
  }
  // The first failure left the window before the fourth came.
  expect(states).toEqual([false, false, false, false, true]);
  jest.advanceTimersByTime(79_999);
  expect(ledger.throttled()).toBe(true);
  jest.advanceTimersByTime(1);
  expect(ledger.throttled()).toBe(false);
});
