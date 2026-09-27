import { afterEach, expect, test } from "bun:test";
import { resourceProtocol } from "./resourceTiming";
import { stubGlobals } from "../../test-helpers.testutil";

const url = "https://meter.test/probe?cb=current";
let restore = () => {};
const originalEntries = performance.getEntriesByName;
let callback: PerformanceObserverCallback;
let disconnected = false;
let observed: PerformanceObserverInit | undefined;

function deferTiming() {
  performance.getEntriesByName = () => [];
  disconnected = false;
  observed = undefined;
  restore = stubGlobals({
    PerformanceObserver: class {
      constructor(receive: PerformanceObserverCallback) {
        callback = receive;
      }
      observe(options: PerformanceObserverInit) {
        observed = options;
      }
      disconnect() {
        disconnected = true;
      }
    },
  });
}

function deliver(name: string, nextHopProtocol: string) {
  callback(
    {
      getEntriesByName: (wanted: string) =>
        wanted === name ? [{ name, nextHopProtocol }] : [],
    } as unknown as PerformanceObserverEntryList,
    {} as PerformanceObserver,
  );
}

afterEach(() => {
  restore();
  performance.getEntriesByName = originalEntries;
});

test("protocol evidence arriving after body completion belongs to the exact probe URL", async () => {
  deferTiming();
  let settled = false;
  const protocol = resourceProtocol(url).then((value) => {
    settled = true;
    return value;
  });
  expect(observed).toEqual({ type: "resource", buffered: true });
  deliver("https://meter.test/probe?cb=older", "h3");
  await Promise.resolve();
  expect(settled).toBe(false);
  deliver(url, "http/1.1");
  expect(await protocol).toBe("http/1.1");
  expect(disconnected).toBe(true);
});

test("already delivered protocol evidence avoids an observer", async () => {
  deferTiming();
  performance.getEntriesByName = () =>
    [{ nextHopProtocol: "h2" }] as unknown as PerformanceEntry[];
  expect(await resourceProtocol(url)).toBe("h2");
  expect(observed).toBeUndefined();
});

test("missing or redacted browser evidence never becomes an inferred protocol", async () => {
  deferTiming();
  expect(await resourceProtocol(url)).toBeUndefined();
  expect(disconnected).toBe(true);
  disconnected = false;
  const protocol = resourceProtocol(url);
  deliver(url, "");
  expect(await protocol).toBeUndefined();
  expect(disconnected).toBe(true);
});

test("cancelling preparation rejects and disconnects the observer", async () => {
  deferTiming();
  const controller = new AbortController();
  const protocol = resourceProtocol(url, controller.signal);
  controller.abort();
  await expect(protocol).rejects.toThrow("abort");
  expect(disconnected).toBe(true);
  observed = undefined;
  await expect(resourceProtocol(url, controller.signal)).rejects.toThrow(
    "abort",
  );
  expect(observed).toBeUndefined();
});
