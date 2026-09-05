import { afterEach, expect, spyOn, test } from "bun:test";
import { resourceProtocol } from "./resourceTiming";

const url = "https://meter.test/probe?cb=current";
const originalObserver = globalThis.PerformanceObserver;
const originalEntries = performance.getEntriesByName;
let callback: PerformanceObserverCallback;
let disconnected = false;
let observed: PerformanceObserverInit | undefined;

function deferTiming() {
  performance.getEntriesByName = () => [];
  disconnected = false;
  observed = undefined;
  globalThis.PerformanceObserver = class {
    constructor(receive: PerformanceObserverCallback) {
      callback = receive;
    }
    observe(options: PerformanceObserverInit) {
      observed = options;
    }
    disconnect() {
      disconnected = true;
    }
  } as unknown as typeof PerformanceObserver;
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
  globalThis.PerformanceObserver = originalObserver;
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

test("cancelling preparation rejects and removes the observer and abort listener", async () => {
  deferTiming();
  const controller = new AbortController();
  const removed = spyOn(controller.signal, "removeEventListener");
  const protocol = resourceProtocol(url, controller.signal);
  controller.abort();
  await expect(protocol).rejects.toThrow("abort");
  expect(disconnected).toBe(true);
  expect(removed).toHaveBeenCalledWith("abort", expect.any(Function));
  removed.mockRestore();
  observed = undefined;
  await expect(resourceProtocol(url, controller.signal)).rejects.toThrow(
    "abort",
  );
  expect(observed).toBeUndefined();
});
