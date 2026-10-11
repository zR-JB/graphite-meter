// Glitch-free push-pull signals: a write marks readers stale, a read recomputes lazily, effects flush once per batch.
// Effects own what they create while running (effects, cleanups) and dispose it before each rerun.

const CLEAN = 0;
const CHECK = 1;
const DIRTY = 2;
type State = typeof CLEAN | typeof CHECK | typeof DIRTY;

type Owner = { owned: (() => void)[] | null };

class Node<T = unknown> implements Owner {
  sources: Node[] = [];
  observers: Node[] = [];
  owned: (() => void)[] | null = null;
  state: State;
  constructor(
    public value: T,
    public fn: (() => T) | null,
    readonly effect: boolean,
  ) {
    this.state = fn ? DIRTY : CLEAN;
  }
}

let owner: Owner | null = null;
let reader: Node | null = null;
let read: Node[] = [];
let depth = 0;
let queue: Node[] = [];

function get<T>(node: Node<T>): T {
  if (reader && !read.includes(node)) read.push(node);
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
  if (node.state === DIRTY) run(node);
  node.state = CLEAN;
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
    for (const source of node.sources) if (!sources.includes(source)) unobserve(source, node);
    for (const source of sources) if (!node.sources.includes(source)) source.observers.push(node);
    node.sources = sources;
  }
  if (!node.effect && !Object.is(previous, node.value)) for (const observer of node.observers) observer.state = DIRTY;
}

function unobserve(source: Node, observer: Node): void {
  const index = source.observers.indexOf(observer);
  if (index >= 0) source.observers.splice(index, 1);
  // A computed nobody reads lets go of its own sources, so it can be collected.
  if (!source.observers.length && source.fn) {
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
  while (queue.length) {
    const effects = queue;
    queue = [];
    for (const effect of effects) if (effect.fn) refresh(effect);
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
  const node = new Node(value, null, false);
  return Object.assign(() => get(node), { peek: () => node.value, set: (next: T) => set(node, next) });
}

export function computed<T>(fn: () => T): Read<T> {
  const node = new Node<T>(undefined as T, fn, false);
  const read = () => get(node);
  return Object.assign(read, { peek: () => untrack(read) });
}

/** Registers `fn` to run when the current owner is disposed or reruns. */
export function onCleanup(fn: () => void): void {
  if (owner) (owner.owned ??= []).push(fn);
}

/** Runs `fn` now and again whenever a signal it read changes; returns its disposer. */
export function effect(fn: () => void): () => void {
  const node = new Node<void>(undefined, fn, true);
  const disposer = () => {
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
  const root: Owner = { owned: null };
  const outer = owner;
  owner = root;
  try {
    return [fn(), () => dispose(root)];
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
