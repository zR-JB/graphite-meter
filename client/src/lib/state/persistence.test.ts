// Persistence tests use an in-memory localStorage and a mocked store default so
// load/merge behavior can be checked without the Svelte runtime.
import { test, expect, mock, beforeEach } from "bun:test";

const FAKE_CONFIG = {
  parallelStreams: 4,
  endpoint: { host: "auto", port: 443 },
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
  expect((result as Record<string, unknown>).somethingMadeUp).toBeUndefined();
  expect((result.config as Record<string, unknown>).bogus).toBeUndefined();
});

test("savePersisted round-trips through loadPersisted", () => {
  const snapshot = defaultPersisted();
  snapshot.dockWidth = { left: 250, right: 500 };
  savePersisted(snapshot);
  expect(loadPersisted()).toEqual(snapshot);
});
