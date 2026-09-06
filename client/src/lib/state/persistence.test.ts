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

const {
  loadPersisted,
  savePersisted,
  defaultPersisted,
  resolveResultHistoryPreference,
  STORAGE_KEY,
} = await import("./persistence");

test("no stored value: returns defaults", () => {
  expect(loadPersisted()).toEqual(defaultPersisted());
});

test("stored value at the current shape: hydrates as-is", () => {
  const snapshot = defaultPersisted();
  snapshot.theme = "light";
  snapshot.unitKind = "bytes";
  expect(loaded(snapshot)).toEqual(snapshot);
});

test("invalid history preference falls back to default and preserves explicit overrides", () => {
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

test("history preference resolves explicit choices over either operator default", () => {
  expect(resolveResultHistoryPreference("default", false)).toBe(false);
  expect(resolveResultHistoryPreference("default", true)).toBe(true);
  expect(resolveResultHistoryPreference("enabled", false)).toBe(true);
  expect(resolveResultHistoryPreference("enabled", true)).toBe(true);
  expect(resolveResultHistoryPreference("disabled", false)).toBe(false);
  expect(resolveResultHistoryPreference("disabled", true)).toBe(false);
});

test("history columns default, validate, deduplicate, and preserve order", () => {
  expect(loadPersisted().historyColumns).toEqual([
    "download",
    "upload",
    "idle",
    "loaded",
  ]);
  expect(
    loaded({
      historyColumns: ["bidirectional", "status", "bidirectional", "bogus"],
    }).historyColumns,
  ).toEqual(["bidirectional"]);
  expect(loaded({ historyColumns: [] }).historyColumns).toEqual(
    defaultPersisted().historyColumns,
  );
});

test("partial stored shape: missing fields fall back to defaults", () => {
  const result = loaded({ theme: "light" });
  expect(result.theme).toBe("light");
  expect(result.unitBase).toBe("base10");
  expect(result.config).toEqual(DEFAULT_CONFIG);
  expect(result.showWireEstimates).toBe(true);
});

test("an explicit wire-estimate opt-out survives hydration", () => {
  expect(loaded({ showWireEstimates: false }).showWireEstimates).toBe(false);
});

test("stored adaptive tuning cannot override internal policy", () => {
  const adaptive = loaded({
    config: {
      adaptive: {
        enabled: false,
        minCoverageRatio: 0.01,
        stabilityThreshold: 0.01,
        maxPhaseReductionRatio: 0.99,
        minLatencySamples: 1,
        minTransferSamples: 1,
        confirmationMs: 1,
        glideMs: 725,
      },
    },
  }).config.adaptive;
  expect(adaptive).toEqual({ ...DEFAULT_CONFIG.adaptive, enabled: false });
  expect(adaptive).not.toHaveProperty("glideMs");
});

test("saving adaptive settings persists only enabled and restores canonical policy", () => {
  const snapshot = defaultPersisted();
  snapshot.config.adaptive = {
    ...snapshot.config.adaptive,
    enabled: false,
    minCoverageRatio: 0.2,
    stabilityThreshold: 0.5,
    maxPhaseReductionRatio: 0.9,
    minLatencySamples: 1,
    minTransferSamples: 1,
    confirmationMs: 10,
  };
  savePersisted(snapshot);
  expect(
    JSON.parse(window.localStorage.getItem(STORAGE_KEY)!).config.adaptive,
  ).toEqual({ enabled: false });
  expect(loadPersisted().config.adaptive).toEqual({
    ...DEFAULT_CONFIG.adaptive,
    enabled: false,
  });
});

test("obsolete ping concurrency is ignored", () => {
  const config = loaded({ config: { pingConcurrency: "slow" } }).config;
  expect(config.pingCadence).toBe(DEFAULT_CONFIG.pingCadence);
  expect(config.loadedPingCadence).toBe("medium");
  expect(config).not.toHaveProperty("pingConcurrency");
});

test("new installations use reply-driven unloaded and medium loaded cadence", () => {
  expect(loadPersisted().config).toMatchObject({
    pingCadence: "reply-driven",
    loadedPingCadence: "medium",
  });
});

test("obsolete instant cadences fall back to current defaults", () => {
  expect(
    loaded({ config: { pingCadence: "instant", loadedPingCadence: "instant" } })
      .config,
  ).toMatchObject({
    pingCadence: "reply-driven",
    loadedPingCadence: "medium",
  });
});

test("obsolete endpoint override cannot restore the old listener port", () => {
  expect(
    loaded({ config: { endpoint: { host: "localhost", port: 8765 } } }).config,
  ).toEqual(DEFAULT_CONFIG);
  expect(loadPersisted().config).not.toHaveProperty("endpoint");
});

test("obsolete parallel-stream ceiling is ignored", () => {
  expect(
    loaded({ config: { parallelStreams: 2 } }).config.transferStreams,
  ).toEqual(DEFAULT_CONFIG.transferStreams);
});

test("obsolete transport role bindings and progress selection are ignored", () => {
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

test("current target identifiers round-trip without historical alias rewriting", () => {
  const snapshot = defaultPersisted();
  snapshot.config.transports = {
    throughputTarget: "http1-clear",
    latencyTarget: "ws-http1-tls",
  };
  snapshot.config.pingCadence = "slow";
  snapshot.config.transferStreams = { mode: "forced", count: 3 };
  savePersisted(snapshot);
  expect(loadPersisted()).toEqual(snapshot);
});

test("latency policy defaults to one server and validates saved preferences", () => {
  expect(loadPersisted().latencySelection).toEqual({
    mode: "primary",
    serverId: "",
  });
  const snapshot = defaultPersisted();
  snapshot.latencySelection = { mode: "all", serverId: "peer" };
  savePersisted(snapshot);
  expect(loadPersisted().latencySelection).toEqual(snapshot.latencySelection);
  expect(
    loaded({ latencySelection: { mode: "corrupt", serverId: 5 } })
      .latencySelection,
  ).toEqual({ mode: "primary", serverId: "" });
  expect(
    loaded({ latencySelection: { mode: "primary", serverId: "x".repeat(129) } })
      .latencySelection.serverId,
  ).toBe("");
});
