import { expect, test } from "bun:test";
import { observeWidth } from "./observeWidth";

test("width observation defers layout writes, coalesces resizes, and cancels on destroy", () => {
  const keys = [
    "ResizeObserver",
    "requestAnimationFrame",
    "cancelAnimationFrame",
  ] as const;
  const previous = keys.map((key) =>
    Object.getOwnPropertyDescriptor(globalThis, key),
  );
  const pending = new Map<number, FrameRequestCallback>();
  let resized = () => {};
  let disconnected = false;
  let sequence = 0;
  const replacements = {
    ResizeObserver: class {
      constructor(callback: () => void) {
        resized = callback;
      }
      observe() {}
      disconnect() {
        disconnected = true;
      }
    },
    requestAnimationFrame: (callback: FrameRequestCallback) => {
      pending.set(++sequence, callback);
      return sequence;
    },
    cancelAnimationFrame: (id: number) => pending.delete(id),
  };
  for (const key of keys)
    Object.defineProperty(globalThis, key, {
      configurable: true,
      value: replacements[key],
    });
  try {
    const node = { clientWidth: 1280 } as HTMLElement;
    const widths: number[] = [];
    const action = observeWidth(node, (width) => widths.push(width));
    expect(widths).toEqual([1280]);
    Object.defineProperty(node, "clientWidth", {
      configurable: true,
      value: 320,
    });
    resized();
    resized();
    expect(widths).toEqual([1280]);
    expect(pending.size).toBe(1);
    const flush = () => {
      const callbacks = [...pending.values()];
      pending.clear();
      for (const callback of callbacks) callback(0);
    };
    flush();
    expect(widths).toEqual([1280, 320]);
    resized();
    flush();
    expect(widths).toEqual([1280, 320]);
    Object.defineProperty(node, "clientWidth", {
      configurable: true,
      value: 640,
    });
    resized();
    expect(pending.size).toBe(1);
    action.destroy();
    expect(disconnected).toBe(true);
    expect(pending.size).toBe(0);
  } finally {
    keys.forEach((key, index) => {
      const descriptor = previous[index];
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else Reflect.deleteProperty(globalThis, key);
    });
  }
});
