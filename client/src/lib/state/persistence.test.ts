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
  parallelStreams: 4,
  experimentalChunkedDownload: false,
  endpoint: { host: "auto", port: 443 },
  compensation: {
    enabled: false,
    profile: "lan",
    transport: "http1-clear",
    factors: {
      ethernetFraming: false,
      encapsulation: false,
      tlsRecords: false,
      applicationFraming: false,
      reversePathControl: false,
      lossRetransmission: false,
      receiverBias: false,
      steadyStateRamp: false,
      browserRuntime: false,
    },
    params: {
      mtuBytes: 1500,
      ipVersion: 4,
      vlanTagged: false,
      tcpOptionsBytes: 12,
      encapsulationBytes: 60,
      framePayloadBytes: 16384,
      tlsRecordBytes: 5,
      aeadTagBytes: 16,
      quicConnIdBytes: 8,
      maxLossRatio: 0.12,
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

test("savePersisted round-trips through loadPersisted", () => {
  const snapshot = defaultPersisted();
  snapshot.dockWidth = { left: 250, right: 500 };
  savePersisted(snapshot);
  expect(loadPersisted()).toEqual(snapshot);
});
