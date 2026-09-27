// In-memory storage and cloned defaults keep persistence tests isolated within Bun.
import { test, expect, beforeEach, afterAll } from "bun:test";
import { DEFAULT_CONFIG } from "./defaults";
import { stubGlobals } from "../test-helpers.testutil";

const stored = new Map<string, string>();
const restore = stubGlobals({
  window: {
    localStorage: {
      getItem: (key: string) => stored.get(key) ?? null,
      setItem: (key: string, value: string) => void stored.set(key, value),
    },
  },
});
afterAll(restore);

const loaded = (value: unknown) => {
  stored.set(STORAGE_KEY, JSON.stringify(value));
  return loadPersisted();
};

beforeEach(() => stored.clear());

const { loadPersisted, savePersisted, defaultPersisted, STORAGE_KEY } =
  await import("./persistence");

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

test("the early-finish switch loads from both saved shapes", () => {
  for (const [adaptive, expected] of [
    [{ enabled: false, minCoverageRatio: 0.01 }, false],
    [false, false],
    [true, true],
    ["yes", DEFAULT_CONFIG.adaptive],
  ] as const)
    expect(loaded({ config: { adaptive } }).config.adaptive).toBe(expected);
});

test("obsolete settings load as current defaults", () => {
  for (const config of [
    { pingConcurrency: "slow" },
    { pingCadence: "instant", loadedPingCadence: "instant" },
    { endpoint: { host: "localhost", port: 8765 } },
    { parallelStreams: 2 },
    {
      transports: {
        transfer: "http3",
        latency: "ws-http1-tls",
        uploadProgress: "ws-http3",
      },
    },
    { compensation: { profile: "internet", params: { mtuBytes: 9000 } } },
  ])
    expect(loaded({ config }).config).toEqual(DEFAULT_CONFIG);
});

test("saved numbers keep their type and stay within bounds", () => {
  const config = loaded({
    config: {
      visualization: { throughputMaxBytesPerSec: 125_000_000 },
      duration: {
        warmupMs: -5,
        latencyMs: 250,
        downloadMs: 1e9,
        uploadMs: "x",
        bidirectionalMs: 2_500,
      },
    },
  }).config;
  expect(config.visualization.throughputMaxBytesPerSec).toBe(125_000_000);
  expect(config.duration).toEqual({
    warmupMs: 0,
    latencyMs: 1_000,
    downloadMs: 300_000,
    uploadMs: DEFAULT_CONFIG.duration.uploadMs,
    bidirectionalMs: 2_500,
  });
});

test("invalid forced stream settings are normalized", () => {
  expect(
    loaded({ config: { transferStreams: { mode: "forced", count: 999.4 } } })
      .config.transferStreams,
  ).toEqual({ mode: "forced", count: 128 });
});

test("corrupt (non-JSON) stored value: falls back to defaults without throwing", () => {
  stored.set(STORAGE_KEY, "{not valid json");
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
