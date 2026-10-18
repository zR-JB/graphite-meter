import { afterAll, beforeAll, expect, spyOn, test } from "bun:test";
import { For } from "./dom";
import { onCleanup, scope, signal } from "./signal";

/** Just enough of the DOM for markers, fragments and text. It counts nodes taken out of a page to be put back. */
class FakeNode {
  static moves = 0;
  parentNode: FakeNode | null = null;
  childNodes: FakeNode[] = [];
  constructor(
    readonly kind: "comment" | "fragment" | "text" | "element",
    readonly data = "",
  ) {}
  get nextSibling(): FakeNode | null {
    const siblings = this.parentNode?.childNodes;
    return siblings?.[siblings.indexOf(this) + 1] ?? null;
  }
  get textContent(): string {
    return this.kind === "text"
      ? this.data
      : this.childNodes.map((node) => node.textContent).join("");
  }
  appendChild(child: FakeNode): FakeNode {
    return this.insertBefore(child, null);
  }
  insertBefore(child: FakeNode, before: FakeNode | null): FakeNode {
    const nodes = child.kind === "fragment" ? [...child.childNodes] : [child];
    for (const node of nodes) {
      if (node.parentNode && node.parentNode.kind !== "fragment") FakeNode.moves++;
      node.remove();
    }
    const at = before ? this.childNodes.indexOf(before) : this.childNodes.length;
    this.childNodes.splice(at, 0, ...nodes);
    for (const node of nodes) node.parentNode = this;
    return child;
  }
  remove(): void {
    const siblings = this.parentNode?.childNodes;
    siblings?.splice(siblings.indexOf(this), 1);
    this.parentNode = null;
  }
}

const globals = globalThis as Record<string, unknown>;
const saved = { document: globals.document, Node: globals.Node };
beforeAll(() => {
  globals.Node = FakeNode;
  globals.document = {
    createComment: () => new FakeNode("comment"),
    createDocumentFragment: () => new FakeNode("fragment"),
    createTextNode: (data: string) => new FakeNode("text", data),
  };
});
afterAll(() => Object.assign(globals, saved));

function list<T>(items: T[], row: (item: T) => string) {
  const each = signal(items);
  const parent = new FakeNode("element");
  const built = { rows: 0, disposed: 0 };
  const [, dispose] = scope(() =>
    parent.appendChild(
      For({
        each,
        by: (item) => item,
        children: (item) => {
          built.rows++;
          onCleanup(() => built.disposed++);
          return new FakeNode("text", row(item())) as unknown as Node;
        },
      }) as unknown as FakeNode,
    ),
  );
  return { each, parent, built, dispose };
}

test("rows that keep their order never move, whatever is added around them", () => {
  const h = list([1, 2, 3], String);
  FakeNode.moves = 0;
  h.each.set([1, 2, 3, 4]);
  h.each.set([0, 1, 2, 3, 4]);
  expect([h.parent.textContent, FakeNode.moves]).toEqual(["01234", 0]);
  h.each.set([4, 0, 1, 2, 3]);
  expect(h.parent.textContent).toBe("40123");
  h.dispose();
  expect(h.built).toEqual({ rows: 5, disposed: 5 });
});

test("a repeated key shows once, and every row built is disposed", () => {
  const h = list([1, 1, 2], String);
  expect(h.parent.textContent).toBe("12");
  h.each.set([]);
  expect([h.parent.textContent, h.built.disposed]).toEqual(["", h.built.rows]);
  h.dispose();
});

test("a row that throws shows empty and the list stays whole", () => {
  const reported = spyOn(globalThis, "reportError").mockImplementation(() => {});
  try {
    const h = list(["a", "bad", "n"], (item) => {
      if (item === "bad") throw new Error("row");
      return item;
    });
    expect(h.parent.textContent).toBe("an");
    h.each.set(["a", "n"]);
    expect(h.parent.textContent).toBe("an");
    h.dispose();
    expect([h.parent.textContent, h.built.disposed]).toEqual(["", h.built.rows]);
    expect(reported).toHaveBeenCalledTimes(1);
  } finally {
    reported.mockRestore();
  }
});
