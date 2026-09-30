import { expect, test } from "bun:test";

// Signals-level regressions for the pinned runtime patch; the browser soak also checks real component teardown.
// @ts-expect-error Svelte intentionally does not publish types for its internal runtime.
import * as runtime from "svelte/internal/client";

interface Signal<T> {
  reactions: unknown[] | null;
  value?: T;
}
const { state, derived, get, set, effect, effect_root, flush, untrack } =
  runtime as {
    state<T>(value: T): Signal<T>;
    derived<T>(read: () => T): Signal<T>;
    get<T>(signal: Signal<T>): T;
    set<T>(signal: Signal<T>, value: T): void;
    effect(fn: () => void): void;
    effect_root(fn: () => void): () => void;
    flush(): void;
    untrack<T>(fn: () => T): T;
  };

test("untracked reads do not reconnect an unused derived chain after its last reader leaves", () => {
  const source = state({ items: [1] });
  const items = derived(() => get(source).items);
  const count = derived(() => get(items).length);
  const snapshot = derived(() => ({ count: get(count) }));
  const show = state(true),
    tick = state(0);
  let seen = 0;
  const destroy = effect_root(() => {
    effect(() => {
      if (get(show))
        effect(() => {
          get(snapshot);
        });
    });
    effect(() => {
      get(tick);
      seen = untrack(() => get(snapshot)).count;
    });
  });
  try {
    flush();
    set(show, false);
    flush();
    set(source, { items: [1, 2] });
    flush();
    set(tick, 1);
    flush();
    expect(seen).toBe(2);
    expect(source.reactions).toBeNull();
  } finally {
    destroy();
    flush();
  }
  expect(source.reactions).toBeNull();
});

test("reconnecting a dirty derived registers each dependency once and releases the whole chain", () => {
  const source = state(0),
    show = state(true);
  const data = derived(() => get(source));
  const items = derived(() => (get(data) ? [get(data)] : []));
  let seen: number[] = [];
  const destroy = effect_root(() => {
    effect(() => {
      if (get(show))
        effect(() => {
          seen = get(items);
        });
    });
  });
  try {
    flush();
    for (let value = 1; value <= 10; value++) {
      set(show, false);
      flush();
      expect(source.reactions).toBeNull();
      set(source, value);
      flush();
      set(show, true);
      flush();
      expect(seen).toEqual([value]);
      expect(data.reactions).toHaveLength(1);
      expect(source.reactions).toHaveLength(1);
    }
  } finally {
    destroy();
    flush();
  }
  expect(source.reactions).toBeNull();
  expect(data.reactions).toBeNull();
});
