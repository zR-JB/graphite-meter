import { expect, test } from "bun:test";
import { focusTrap } from "./focusTrap";

function fixture(
  run: (fixture: {
    node: HTMLElement;
    pending: Map<number, () => void>;
    focused: string[];
    replace: () => void;
  }) => void,
) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  const pending = new Map<number, () => void>();
  let sequence = 0;
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      setTimeout: (callback: () => void) => {
        pending.set(++sequence, callback);
        return sequence;
      },
      clearTimeout: (id: number) => pending.delete(id),
    },
  });
  const focused: string[] = [];
  const element = (name: string) => ({
    isConnected: true,
    closest: () => null,
    matches: () => false,
    checkVisibility: () => true,
    focus: () => focused.push(name),
  });
  let first = element("first");
  const node = {
    ...element("dialog"),
    querySelectorAll: () => [first],
    addEventListener() {},
    removeEventListener() {},
  } as unknown as HTMLElement;
  try {
    run({
      node,
      pending,
      focused,
      replace: () => {
        first = element("replacement");
      },
    });
  } finally {
    if (previous) Object.defineProperty(globalThis, "window", previous);
    else Reflect.deleteProperty(globalThis, "window");
  }
}

test("deactivating or destroying a trap cancels its pending focus", () => {
  fixture(({ node, pending, focused }) => {
    const trap = focusTrap(node);
    expect(pending.size).toBe(1);
    trap.update(false);
    expect(pending.size).toBe(0);
    trap.update(true);
    expect(pending.size).toBe(1);
    trap.destroy();
    expect(pending.size).toBe(0);
    expect(focused).toEqual([]);
  });
});

test("the pending trap resolves current contents once without refocusing on repeated activation", () => {
  fixture(({ node, pending, focused, replace }) => {
    const trap = focusTrap(node);
    trap.update(true);
    expect(pending.size).toBe(1);
    replace();
    pending.values().next().value!();
    expect(focused).toEqual(["replacement"]);
    pending.clear();
    trap.update(true);
    expect(pending.size).toBe(0);
    trap.destroy();
  });
});

test("a disconnected trap does not steal focus when its opening callback runs", () => {
  fixture(({ node, pending, focused }) => {
    const trap = focusTrap(node);
    Object.defineProperty(node, "isConnected", { value: false });
    pending.values().next().value!();
    expect(focused).toEqual([]);
    trap.destroy();
  });
});
