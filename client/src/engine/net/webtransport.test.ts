import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { HandshakeLedger } from "./webtransport";

beforeEach(() => jest.useFakeTimers());
afterEach(() => jest.useRealTimers());

test("eight failed handshakes within 5 minutes throttle WebTransport until the oldest of them is 5 minutes old", () => {
  const ledger = new HandshakeLedger();
  for (let i = 0; i < 7; i++) {
    ledger.failed();
    expect(ledger.throttled()).toBe(false);
  }
  jest.advanceTimersByTime(10_000);
  ledger.failed();
  expect(ledger.throttled()).toBe(true);
  jest.advanceTimersByTime(289_999);
  expect(ledger.throttled()).toBe(true);
  jest.advanceTimersByTime(1);
  expect(ledger.throttled()).toBe(false);
});
