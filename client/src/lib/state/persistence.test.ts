// Persistence tests use an in-memory localStorage and a mocked store default so
// load/merge behavior can be checked without the Svelte runtime.
import { test, expect, mock, beforeEach } from "bun:test";
import type { RunnerConfig } from "../runner/contract";

const FAKE_CONFIG: RunnerConfig = {
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: {
    warmupMs: 800,
    latencyMs: 4000,
    downloadMs: 10000,
    uploadMs: 10000,
    bidirectionalMs: 10000,
  },
  pingConcurrency: "medium",
  transferStreams: { mode: "auto", count: 6 },
  experimentalChunkedDownload: false,
  endpoint: { host: "auto", port: 443 },
  transports: { transfer: "current", latency: "auto", uploadProgress: "auto" },
  compensation: {
    profile: "lan",
    transport: "auto",
    params: {
      mtuBytes: 1500,
      ipVersion: "auto",
      vlanTagged: false,
      tcpOptionsMinBytes: 0,
      tcpOptionsMaxBytes: 12,
      encapsulationBytes: 0,
      quicConnIdMinBytes: 0,
      quicConnIdMaxBytes: 20,
    },
  },
  adaptive: {
    enabled: false,
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
    glideMs: 1100,
  },
  visualization: { throughputMaxBytesPerSec: "auto" },
};
mock.module("./store.svelte", () => ({ DEFAULT_CONFIG: FAKE_CONFIG }));

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? this.map.get(key)! : null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  removeItem(key: string): void {
    this.map.delete(key);
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
  expect(result.config).toEqual(FAKE_CONFIG);
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
    transfer: "http1-clear",
    latency: "auto",
    uploadProgress: "auto",
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

test("savePersisted round-trips through loadPersisted", () => {
  const snapshot = defaultPersisted();
  snapshot.dockWidth = { left: 250, right: 500 };
  savePersisted(snapshot);
  expect(loadPersisted()).toEqual(snapshot);
});
