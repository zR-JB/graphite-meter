// Persistence tests use an in-memory localStorage and the shipped config
// defaults, so this file cannot poison the shared defaults module for other
// tests in the Bun process.
import { test, expect, beforeEach } from "bun:test";
import { DEFAULT_CONFIG } from "./defaults";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? this.map.get(key)! : null;
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
  memoryStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot));
  expect(loadPersisted()).toEqual(snapshot);
});

test("older/partial stored shape: missing fields fall back to defaults", () => {
  memoryStorage.setItem(STORAGE_KEY, JSON.stringify({ theme: "light" }));
  const result = loadPersisted();
  expect(result.theme).toBe("light");
  expect(result.unitBase).toBe("base10");
  expect(result.config).toEqual(DEFAULT_CONFIG);
  expect(result.showWireEstimates).toBe(true);
});

test("an explicit wire-estimate opt-out survives hydration", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ showWireEstimates: false }),
  );
  expect(loadPersisted().showWireEstimates).toBe(false);
});

test("legacy glide duration migrates once into confirmation duration", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ config: { adaptive: { glideMs: 725 } } }),
  );
  expect(loadPersisted().config.adaptive.confirmationMs).toBe(725);
  expect(loadPersisted().config.adaptive).not.toHaveProperty("glideMs");
});

test("confirmation duration wins when both old and new fields exist", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      config: { adaptive: { glideMs: 725, confirmationMs: 900 } },
    }),
  );
  expect(loadPersisted().config.adaptive.confirmationMs).toBe(900);
});

test("legacy ping concurrency becomes unloaded cadence with the new loaded default", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ config: { pingConcurrency: "slow" } }),
  );
  const config = loadPersisted().config;
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
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      config: { pingCadence: "instant", loadedPingCadence: "instant" },
    }),
  );
  expect(loadPersisted().config).toMatchObject({
    pingCadence: "reply-driven",
    loadedPingCadence: "reply-driven",
  });
});

test("obsolete endpoint override cannot restore the old listener port", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      config: { endpoint: { host: "localhost", port: 8765 } },
    }),
  );
  expect(loadPersisted().config).toEqual(DEFAULT_CONFIG);
  expect(loadPersisted().config).not.toHaveProperty("endpoint");
});

test("legacy parallel-stream ceiling migrates into automatic policy", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ config: { parallelStreams: 2 } }),
  );
  expect(loadPersisted().config.transferStreams).toEqual({
    mode: "auto",
    count: 2,
  });
});

test("legacy protocol selection migrates to a transfer target", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ config: { endpoint: { protocol: "http1" } } }),
  );
  expect(loadPersisted().config.transports).toEqual({
    throughputTarget: "auto",
    latencyTarget: "auto",
  });
});

test("legacy role bindings migrate and obsolete progress selection is dropped", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      config: {
        transports: {
          transfer: "http3",
          latency: "ws-http1-tls",
          uploadProgress: "ws-http3",
        },
      },
    }),
  );
  expect(loadPersisted().config.transports).toEqual({
    throughputTarget: "auto",
    latencyTarget: "auto",
  });
});

test("invalid forced stream settings are normalized", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      config: { transferStreams: { mode: "forced", count: 999.4 } },
    }),
  );
  expect(loadPersisted().config.transferStreams).toEqual({
    mode: "forced",
    count: 128,
  });
});

test("corrupt (non-JSON) stored value: falls back to defaults without throwing", () => {
  memoryStorage.setItem(STORAGE_KEY, "{not valid json");
  expect(() => loadPersisted()).not.toThrow();
  expect(loadPersisted()).toEqual(defaultPersisted());
});

test("unknown/extra stored keys: dropped, known keys still merge", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      theme: "dark",
      somethingMadeUp: 123,
      config: { bogus: true },
    }),
  );
  const result = loadPersisted();
  expect(result.theme).toBe("dark");
  expect(
    (result as unknown as Record<string, unknown>).somethingMadeUp,
  ).toBeUndefined();
  expect(
    (result.config as unknown as Record<string, unknown>).bogus,
  ).toBeUndefined();
});

test("obsolete compensation presets and factors cannot survive hydration", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      config: {
        compensation: {
          profile: "internet",
          factors: { browserRuntime: true, lossRetransmission: true },
        },
      },
    }),
  );
  const compensation = loadPersisted().config.compensation;
  expect(compensation.profile).toBe("lan");
  expect("factors" in compensation).toBe(false);
});

test("legacy numeric IP family remains an expert override", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      config: { compensation: { params: { ipVersion: 6 } } },
    }),
  );
  expect(loadPersisted().config.compensation.params.ipVersion).toBe(6);
});

// The merge only checks that a leaf keeps its type, so a tab name this build no
// longer has survives it and leaves the settings panel with no tab selected.
test("a settings tab this build does not have falls back to setup", () => {
  memoryStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ settingsTab: "advanced" }),
  );
  expect(loadPersisted().settingsTab).toBe("setup");
});

test("savePersisted round-trips through loadPersisted", () => {
  const snapshot = defaultPersisted();
  snapshot.dockWidth = { left: 250, right: 500 };
  savePersisted(snapshot);
  expect(loadPersisted()).toEqual(snapshot);
});
