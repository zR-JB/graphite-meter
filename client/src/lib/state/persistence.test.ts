// In-memory storage and cloned defaults keep persistence tests isolated within Bun.
import { test, expect, beforeEach } from "bun:test";
import { DEFAULT_CONFIG } from "./defaults";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  clear(): void {
    this.map.clear();
  }
}
const memoryStorage = new MemoryStorage();
(globalThis as { window?: unknown }).window = { localStorage: memoryStorage };

const loaded = (value: unknown) => {
  memoryStorage.setItem(STORAGE_KEY, JSON.stringify(value));
  return loadPersisted();
};

beforeEach(() => {
  memoryStorage.clear();
});

const { loadPersisted, savePersisted, defaultPersisted, STORAGE_KEY } =
  await import("./persistence");

test("no stored value: returns defaults", () => {
  expect(loadPersisted()).toEqual(defaultPersisted());
});

test("stored value at the current shape: hydrates as-is", () => {
  const snapshot = defaultPersisted();
  snapshot.theme = "light";
  snapshot.unitKind = "bytes";
  expect(loaded(snapshot)).toEqual(snapshot);
});

test("history preference migrates to default and preserves explicit overrides", () => {
  const snapshot = defaultPersisted();
  snapshot.resultHistoryPreference = "enabled";
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot));
  expect(loadPersisted().resultHistoryPreference).toBe("enabled");
  window.localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ ...snapshot, resultHistoryPreference: "corrupt" }),
  );
  expect(loadPersisted().resultHistoryPreference).toBe("default");
});

test("history columns default, validate, deduplicate, and preserve order", () => {
  expect(loadPersisted().historyColumns).toEqual([
    "download",
    "upload",
    "idle",
    "loaded",
    "status",
  ]);
  expect(
    loaded({
      historyColumns: ["status", "bidirectional", "status", "bogus"],
    }).historyColumns,
  ).toEqual(["status", "bidirectional"]);
  expect(loaded({ historyColumns: [] }).historyColumns).toEqual(
    defaultPersisted().historyColumns,
  );
});

test("older/partial stored shape: missing fields fall back to defaults", () => {
  const result = loaded({ theme: "light" });
  expect(result.theme).toBe("light");
  expect(result.unitBase).toBe("base10");
  expect(result.config).toEqual(DEFAULT_CONFIG);
  expect(result.showWireEstimates).toBe(true);
});

test("an explicit wire-estimate opt-out survives hydration", () => {
  expect(loaded({ showWireEstimates: false }).showWireEstimates).toBe(false);
});

test("legacy glide duration migrates once into confirmation duration", () => {
  expect(
    loaded({ config: { adaptive: { glideMs: 725 } } }).config.adaptive
      .confirmationMs,
  ).toBe(725);
  expect(loadPersisted().config.adaptive).not.toHaveProperty("glideMs");
});

test("confirmation duration wins when both old and new fields exist", () => {
  expect(
    loaded({ config: { adaptive: { glideMs: 725, confirmationMs: 900 } } })
      .config.adaptive.confirmationMs,
  ).toBe(900);
});

test("legacy ping concurrency becomes unloaded cadence with the new loaded default", () => {
  const config = loaded({ config: { pingConcurrency: "slow" } }).config;
  expect(config.pingCadence).toBe("slow");
  expect(config.loadedPingCadence).toBe("medium");
  expect(config).not.toHaveProperty("pingConcurrency");
});

test("new installations use reply-driven unloaded and medium loaded cadence", () => {
  expect(loadPersisted().config).toMatchObject({
    pingCadence: "reply-driven",
    loadedPingCadence: "medium",
  });
});

test("old instant cadences migrate to reply-driven", () => {
  expect(
    loaded({ config: { pingCadence: "instant", loadedPingCadence: "instant" } })
      .config,
  ).toMatchObject({
    pingCadence: "reply-driven",
    loadedPingCadence: "reply-driven",
  });
});

test("obsolete endpoint override cannot restore the old listener port", () => {
  expect(
    loaded({ config: { endpoint: { host: "localhost", port: 8765 } } }).config,
  ).toEqual(DEFAULT_CONFIG);
  expect(loadPersisted().config).not.toHaveProperty("endpoint");
});

test("legacy parallel-stream ceiling migrates into automatic policy", () => {
  expect(
    loaded({ config: { parallelStreams: 2 } }).config.transferStreams,
  ).toEqual({ mode: "auto", count: 2 });
});

test("legacy role bindings migrate and obsolete progress selection is dropped", () => {
  expect(
    loaded({
      config: {
        transports: {
          transfer: "http3",
          latency: "ws-http1-tls",
          uploadProgress: "ws-http3",
        },
      },
    }).config.transports,
  ).toEqual({ throughputTarget: "auto", latencyTarget: "auto" });
});

test("invalid forced stream settings are normalized", () => {
  expect(
    loaded({ config: { transferStreams: { mode: "forced", count: 999.4 } } })
      .config.transferStreams,
  ).toEqual({ mode: "forced", count: 128 });
});

test("corrupt (non-JSON) stored value: falls back to defaults without throwing", () => {
  memoryStorage.setItem(STORAGE_KEY, "{not valid json");
  expect(() => loadPersisted()).not.toThrow();
  expect(loadPersisted()).toEqual(defaultPersisted());
});

test("unknown/extra stored keys: dropped, known keys still merge", () => {
  const result = loaded({
    theme: "dark",
    somethingMadeUp: 123,
    config: { bogus: true },
  });
  expect(result.theme).toBe("dark");
  expect(
    (result as unknown as Record<string, unknown>).somethingMadeUp,
  ).toBeUndefined();
  expect(
    (result.config as unknown as Record<string, unknown>).bogus,
  ).toBeUndefined();
});

test("obsolete compensation settings are discarded without hydration", () => {
  const config = loaded({
    config: {
      compensation: {
        profile: "internet",
        transport: "http3-quic",
        params: { mtuBytes: 9000, ipVersion: 6 },
      },
    },
  }).config;
  expect(config).not.toHaveProperty("compensation");
});

test("savePersisted round-trips through loadPersisted", () => {
  const snapshot = defaultPersisted();
  snapshot.dockWidth = { left: 250, right: 500 };
  savePersisted(snapshot);
  expect(loadPersisted()).toEqual(snapshot);
});
