import { afterAll, beforeAll, expect, test } from "bun:test";
import { jsx } from "./jsx-runtime";
import { signal } from "./signal";

/** An element that records each attribute write and each value assignment. */
class FakeElement {
  writes: string[] = [];
  #value = "";
  setAttribute(name: string, value: string) {
    this.writes.push(`${name}=${value}`);
  }
  removeAttribute(name: string) {
    this.writes.push(`-${name}`);
  }
  addEventListener() {}
  appendChild<T>(child: T) {
    return child;
  }
  get value() {
    return this.#value;
  }
  set value(value: string) {
    this.writes.push(`.value=${value}`);
    this.#value = value;
  }
}

const original = globalThis.document;
beforeAll(() => {
  Object.assign(globalThis, { document: { createElement: () => new FakeElement() } });
});
afterAll(() => {
  Object.assign(globalThis, { document: original });
});

test("a bound attribute is written only when its value changes; a field's value every time", () => {
  const word = signal("a");
  const other = signal(0);
  const element = jsx("div", {
    class: () => (other(), word()),
    value: () => (other(), word()),
  }) as unknown as FakeElement;
  other.set(1);
  word.set("b");
  word.set("c");
  other.set(2);
  expect(element.writes).toEqual([
    "class=a",
    ".value=a",
    ".value=a",
    "class=b",
    ".value=b",
    "class=c",
    ".value=c",
    ".value=c",
  ]);
});
