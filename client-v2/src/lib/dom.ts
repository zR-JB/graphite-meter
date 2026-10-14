// Lists and conditional content for the JSX runtime: rows and branches are built once and kept or moved, never
// rebuilt for changes that do not concern them.
import { clear, insert, type Child } from "./jsx-runtime";
import { computed, effect, onCleanup, scope, signal, untrack, type Read, type Signal } from "./signal";

/** Content that rebuilds only when `when` turns truthy or falsy; the branch reads the current value. */
export function Show<T>(props: {
  when: () => T;
  children: (value: Read<NonNullable<T>>) => Child;
  fallback?: () => Child;
}): Node {
  const value = computed(props.when);
  const shown = computed(() => !!value());
  const fragment = document.createDocumentFragment();
  insert(fragment, () =>
    shown() ? untrack(() => props.children(value as Read<NonNullable<T>>)) : untrack(() => props.fallback?.()),
  );
  return fragment;
}

interface Row<T> {
  item: Signal<T>;
  index: Signal<number>;
  /** The row's content lies between these markers. */
  start: Comment;
  end: Comment;
  dispose: () => void;
}

/** A keyed list. Each row is built once, follows its item through signals, and moves as the order changes. */
export function For<T>(props: {
  each: () => readonly T[];
  /** The row's identity. (Not `key`: the JSX transform keeps that name for itself.) */
  by: (item: T) => unknown;
  children: (item: Read<T>, index: Read<number>) => Child;
}): Node {
  const fragment = document.createDocumentFragment();
  const head = fragment.appendChild(document.createComment(""));
  let rows = new Map<unknown, Row<T>>();
  effect(() => {
    const items = props.each();
    untrack(() => {
      const next = new Map<unknown, Row<T>>();
      items.forEach((item, index) => {
        const key = props.by(item);
        // A repeated key shows once.
        if (next.has(key)) return;
        let row = rows.get(key);
        if (row) {
          row.item.set(item);
          row.index.set(index);
        } else {
          const state = { item: signal(item), index: signal(index) };
          const content = document.createDocumentFragment();
          const start = content.appendChild(document.createComment(""));
          // A row that cannot build shows empty; the list stays whole.
          const [, dispose] = scope(() => {
            try {
              insert(content, props.children(state.item, state.index));
            } catch (error) {
              reportError(error);
            }
          });
          const end = content.appendChild(document.createComment(""));
          // It waits in its own fragment until the pass below puts it in place.
          row = { ...state, start, end, dispose };
        }
        next.set(key, row);
      });
      for (const [key, row] of rows) if (!next.has(key)) remove(row);
      rows = next;
      // Only rows out of place move.
      let previous: Node = head;
      for (const row of rows.values()) {
        if (previous.nextSibling !== row.start) move(row, previous);
        previous = row.end;
      }
    });
  });
  onCleanup(() => {
    for (const row of rows.values()) remove(row);
    rows.clear();
  });
  return fragment;
}

/** Moves a row's markers and everything between them after `previous`. */
function move<T>(row: Row<T>, previous: Node): void {
  const range = document.createDocumentFragment();
  for (let node: Node | null = row.start; node; ) {
    const next: Node | null = node === row.end ? null : node.nextSibling;
    range.appendChild(node);
    node = next;
  }
  previous.parentNode!.insertBefore(range, previous.nextSibling);
}

function remove<T>(row: Row<T>): void {
  row.dispose();
  clear(row.start, row.end);
  row.start.remove();
  row.end.remove();
}
