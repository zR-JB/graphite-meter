import { expect, test } from "bun:test";
import { STORAGE_KEY } from "../../auth/theme";
import { createPreferences, DEFAULT_CONFIG, readPreferences } from "./preferences";

test("stored preferences are read field by field, older shapes migrate and unknown keys go", () => {
  const read = readPreferences({
    config: {
      stages: { latency: true, download: true, upload: "yes" },
      // Earlier versions skipped a stage with 0 ms and saved early finish as { enabled }.
      duration: { warmupMs: 99_999, latencyMs: 0, downloadMs: 2_500.4, uploadMs: Number.NaN },
      pingCadence: "instant",
      transferStreams: { mode: "forced", count: 500 },
      transports: { throughputTarget: "x".repeat(257), latencyTarget: "transport:websocket" },
      adaptive: { enabled: false },
      visualization: { throughputMaxBytesPerSec: 0.5 },
      parallelStreams: 8,
    },
    theme: "sepia",
    historyColumns: ["idle", "idle", "made-up", "upload"],
    dockWidth: { left: 900, right: -3 },
    historySplit: 1,
    latencySelection: "old",
  });
  expect(read.config).toEqual({
    ...DEFAULT_CONFIG,
    stages: { latency: false, download: true, upload: true, bidirectional: false },
    duration: { warmupMs: 4_000, latencyMs: 4_000, downloadMs: 2_500, uploadMs: 10_000, bidirectionalMs: 10_000 },
    transferStreams: { mode: "forced", count: 128 },
    transports: { throughputTarget: "auto", latencyTarget: "transport:websocket" },
    adaptive: false,
  });
  expect(read).toMatchObject({
    theme: "auto",
    historyColumns: ["idle", "upload"],
    dockWidth: { left: 900, right: 420 },
    historySplit: 1,
  });
  expect(Object.keys(read)).not.toContain("latencySelection");
  expect(readPreferences("{corrupt")).toEqual(readPreferences(undefined));
});

function storage(): Storage {
  const items = new Map<string, string>();
  return {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
  } as Storage;
}
const stored = (shared: Storage) => JSON.parse(shared.getItem(STORAGE_KEY)!);

test("two tabs never take back each other's choices", () => {
  const shared = storage();
  const [a, b] = [new EventTarget(), new EventTarget()];
  const tabA = createPreferences(shared, a);
  const tabB = createPreferences(shared, b);
  for (const tab of [a, b]) tab.dispatchEvent(new Event("pagehide"));
  tabA.set({ theme: "dark" });
  a.dispatchEvent(new Event("pagehide"));
  tabB.set({ unitKind: "bytes" });
  b.dispatchEvent(new Event("pagehide"));
  expect(stored(shared)).toMatchObject({ theme: "dark", unitKind: "bytes" });
  expect(Object.keys(stored(shared))[0]).toBe("config");

  // Only the History choices follow another tab, and following one writes nothing back.
  tabA.set({ resultHistoryPreference: "enabled", unitBase: "base2" });
  a.dispatchEvent(new Event("pagehide"));
  b.dispatchEvent(Object.assign(new Event("storage"), { key: STORAGE_KEY }));
  expect(tabB.resultHistoryPreference()).toBe("enabled");
  expect(tabB.unitBase()).toBe("base10");
  shared.setItem(STORAGE_KEY, JSON.stringify({ ...stored(shared), keyShortcuts: false }));
  b.dispatchEvent(new Event("pagehide"));
  expect(stored(shared).keyShortcuts).toBe(false);
  tabA.dispose();
  tabB.dispose();
});
