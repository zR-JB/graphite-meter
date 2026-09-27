import "./runes.testutil";
import { expect, jest, test } from "bun:test";
import { flushSync } from "svelte";
import { stubGlobals } from "../test-helpers.testutil";
import { TEST_BUILD_TOKENS } from "../runner/test-helpers.testutil";
import { STORAGE_KEY } from "./persistence";

test("another tab's unchanged settings are never written back", async () => {
  const storage = new Map<string, string>();
  const handlers = new Map<string, (event: { key: string }) => void>();
  let writes = 0;
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    window: {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => {
          writes++;
          storage.set(key, value);
        },
      },
      addEventListener: (type: string, handler: () => void) =>
        handlers.set(type, handler),
      removeEventListener() {},
    },
    document: { documentElement: { setAttribute() {} } },
  });
  const { store, mountStoreEffects } = await import("./store.svelte");
  jest.useFakeTimers();
  const unmount = mountStoreEffects(store);
  try {
    flushSync();
    await Promise.resolve();
    jest.advanceTimersByTime(300);
    const saved = writes;
    expect(saved).toBeGreaterThan(0);
    handlers.get("storage")!({ key: STORAGE_KEY });
    flushSync();
    await Promise.resolve();
    jest.advanceTimersByTime(300);
    expect(writes).toBe(saved);
  } finally {
    unmount();
    jest.useRealTimers();
    restore();
  }
});
