// Glitch-free push-pull signals: a write marks readers stale, a read recomputes lazily, effects flush once per batch,
// owners before the effects they own. Effects own what they create while running (effects, cleanups) and dispose it
// before each rerun. An exception in one effect is reported and never stops the others.

const CLEAN = 0;
const CHECK = 1;
const DIRTY = 2;
type State = typeof CLEAN | typeof CHECK | typeof DIRTY;

interface Owner {
  owned: (() => void)[] | null;
  /** How deep in the ownership tree: owners run before what they own. */
  depth: number;
  /** Disposed for good: a cleanup registered later runs at once. */
  disposed: boolean;
}

class Node<T = unknown> implements Owner {
  sources: Node[] = [];
  observers: Node[] = [];
  owned: (() => void)[] | null = null;
  disposed = false;
  state: State;
  constructor(
    public value: T,
    public fn: (() => T) | null,
    readonly effect: boolean,
    readonly depth: number,
  ) {
    this.state = fn ? DIRTY : CLEAN;
  }
}

/** Writes from effects that keep re-triggering each other end here rather than hanging the page. */
const MAX_ROUNDS = 100_000;

let owner: Owner | null = null;
let reader: Node | null = null;
let read: Node[] = [];
let depth = 0;
let queue: Node[] = [];

const report = (error: unknown) =>
  (
    globalThis.reportError ??
    ((e) =>
      queueMicrotask(() => {
        throw e;
      }))
  )(error);

function get<T>(node: Node<T>): T {
  // A reader subscribes on its first read, so a write it causes later in the same run reaches it.
  if (reader && !read.includes(node)) {
    read.push(node);
    if (!reader.sources.includes(node)) node.observers.push(reader);
  }
  if (node.fn) refresh(node);
  return node.value;
}

function set<T>(node: Node<T>, value: T): void {
  if (Object.is(node.value, value)) return;
  node.value = value;
  depth++;
  for (const observer of node.observers) stale(observer, DIRTY);
  flush();
}

function stale(node: Node, state: State): void {
  if (node.state >= state) return;
  if (node.state === CLEAN && node.effect) queue.push(node);
  node.state = state;
  for (const observer of node.observers) stale(observer, CHECK);
}

function refresh(node: Node): void {
  if (node.state === CHECK)
    for (const source of node.sources) {
      if (source.fn) refresh(source);
      if ((node.state as State) === DIRTY) break;
    }
  if (node.state !== DIRTY) {
    node.state = CLEAN;
    return;
  }
  // Clean before it runs, so a write it causes to its own sources marks it stale again.
  node.state = CLEAN;
  run(node);
}

function run(node: Node): void {
  dispose(node);
  const previous = node.value;
  const [outerOwner, outerReader, outerRead] = [owner, reader, read];
  [owner, reader, read] = [node, node, []];
  try {
    node.value = node.fn!();
  } finally {
    const sources = read;
    [owner, reader, read] = [outerOwner, outerReader, outerRead];
    // New sources subscribed as they were read, so letting go of old ones never releases one this run still reads.
    for (const source of node.sources) if (!sources.includes(source)) unobserve(source, node);
    node.sources = sources;
    if (node.disposed) {
      for (const source of sources) unobserve(source, node);
      node.sources = [];
    }
  }
  // A changed value settles the readers that were checking it; one reading it right now already has it.
  if (!node.effect && !Object.is(previous, node.value))
    for (const observer of node.observers) if (observer.state === CHECK) observer.state = DIRTY;
}

function unobserve(source: Node, observer: Node): void {
  const index = source.observers.indexOf(observer);
  if (index >= 0) source.observers.splice(index, 1);
  // A computed nobody reads lets go of its own sources, so it can be collected.
  if (!source.observers.length && source.fn && !source.effect) {
    for (const upstream of source.sources) unobserve(upstream, source);
    source.sources = [];
    source.state = DIRTY;
  }
}

function dispose(target: Owner): void {
  const owned = target.owned;
  target.owned = null;
  if (owned) for (let i = owned.length - 1; i >= 0; i--) owned[i]!();
}

function flush(): void {
  if (--depth) return;
  depth++;
  try {
    for (let rounds = 0; queue.length; rounds++) {
      if (rounds > MAX_ROUNDS) {
        queue = [];
        report(new Error("Effects kept re-triggering each other"));
        break;
      }
      const effects = queue.sort((a, b) => a.depth - b.depth);
      queue = [];
      for (const effect of effects)
        if (effect.fn && effect.state !== CLEAN)
          try {
            refresh(effect);
          } catch (error) {
            report(error);
          }
    }
  } finally {
    depth--;
  }
}

/** A readable signal: call it to read and subscribe, `peek()` to read without subscribing. */
export interface Read<T> {
  (): T;
  peek(): T;
}
export interface Signal<T> extends Read<T> {
  set(value: T): void;
}

export function signal<T>(value: T): Signal<T> {
  const node = new Node(value, null, false, 0);
  return Object.assign(() => get(node), { peek: () => node.value, set: (next: T) => set(node, next) });
}

export function computed<T>(fn: () => T): Read<T> {
  const node = new Node<T>(undefined as T, fn, false, 0);
  const read = () => get(node);
  return Object.assign(read, { peek: () => untrack(read) });
}

/** Registers `fn` to run when the current owner is disposed or reruns. */
export function onCleanup(fn: () => void): void {
  if (owner?.disposed) fn();
  else if (owner) (owner.owned ??= []).push(fn);
}

/** Runs `fn` now and again whenever a signal it read changes; returns its disposer. */
export function effect(fn: () => void): () => void {
  // An owner already gone starts nothing.
  if (owner?.disposed) return () => {};
  const node = new Node<void>(undefined, fn, true, owner ? owner.depth + 1 : 0);
  const disposer = () => {
    node.disposed = true;
    dispose(node);
    node.fn = null;
    for (const source of node.sources) unobserve(source, node);
    node.sources = [];
  };
  onCleanup(disposer);
  depth++;
  try {
    refresh(node);
  } finally {
    flush();
  }
  return disposer;
}

/** Runs `fn` as an owner of its own: effects and cleanups it creates live until the returned disposer runs. */
export function scope<T>(fn: () => T): [T, () => void] {
  const root: Owner = { owned: null, depth: owner ? owner.depth + 1 : 0, disposed: false };
  const outer = owner;
  owner = root;
  try {
    return [
      fn(),
      () => {
        root.disposed = true;
        dispose(root);
      },
    ];
  } finally {
    owner = outer;
  }
}

export function batch<T>(fn: () => T): T {
  depth++;
  try {
    return fn();
  } finally {
    flush();
  }
}

export function untrack<T>(fn: () => T): T {
  const outer = reader;
  reader = null;
  try {
    return fn();
  } finally {
    reader = outer;
  }
}
