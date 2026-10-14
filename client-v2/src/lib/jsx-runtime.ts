// JSX that builds real DOM: a function in an attribute or child binds that spot to the signals it reads.
import { effect, untrack } from "./signal";

type Primitive = string | number | boolean | null | undefined;
type Reactive<T> = T | (() => T);
export type Child = Node | Primitive | readonly Child[] | (() => Child);

type Handlers<E, M> = {
  [K in keyof M as `on${K & string}`]?: (event: M[K] & { currentTarget: E }) => void;
};
type Attributes<E> = {
  [name: string]: unknown;
  children?: Child;
  ref?: (element: E) => void;
  class?: Reactive<string | false | null | undefined>;
  style?: Reactive<string | undefined>;
  value?: Reactive<string | number>;
  checked?: Reactive<boolean>;
};
type Props<E, M> = Attributes<E> & Handlers<E, M>;

const SVG_NS = "http://www.w3.org/2000/svg";
// Children are built before their parent, so a tag's own name decides its namespace.
const SVG_TAGS = new Set([
  "svg",
  "g",
  "path",
  "circle",
  "line",
  "rect",
  "polyline",
  "polygon",
  "defs",
  "clipPath",
  "mask",
  "linearGradient",
  "radialGradient",
  "stop",
  "pattern",
  "use",
  "text",
  "tspan",
  "textPath",
  "marker",
  "ellipse",
  "image",
  "symbol",
  "filter",
  "foreignObject",
]);
// Live state that an attribute only seeds.
const PROPERTIES = new Set(["value", "checked"]);

type Component<P> = (props: P) => Node;

export function jsx(type: string | Component<any>, props: Record<string, unknown>): Node {
  if (typeof type === "function") return untrack(() => type(props));
  const element = SVG_TAGS.has(type) ? document.createElementNS(SVG_NS, type) : document.createElement(type);
  // Children first, so a select's value finds its option; the ref last, so it sees the finished element.
  insert(element, props.children as Child);
  for (const name in props) {
    const value = props[name];
    if (name === "children" || name === "ref") continue;
    if (name.startsWith("on")) element.addEventListener(name.slice(2), value as EventListener);
    else if (typeof value === "function") bind(element, name, value as () => Primitive);
    else assign(element, name, value as Primitive);
  }
  (props.ref as ((element: Element) => void) | undefined)?.(element);
  return element;
}
export { jsx as jsxs, jsx as jsxDEV };

export function Fragment(props: { children?: Child }): Node {
  const fragment = document.createDocumentFragment();
  insert(fragment, props.children);
  return fragment;
}

// A rerun that yields the attribute it wrote last leaves the element alone, as every write restyles it. Live state
// (a field's value, a box's checked) is written each time, so a rerun can take back what the user changed.
function bind(element: Element, name: string, read: () => Primitive): void {
  if (name === "value" || PROPERTIES.has(name)) return void effect(() => assign(element, name, read()));
  let written: { value: Primitive } | null = null;
  effect(() => {
    const value = read();
    if (written && Object.is(written.value, value)) return;
    written = { value };
    assign(element, name, value);
  });
}

function assign(element: Element, name: string, value: Primitive): void {
  if (name === "value") (element as HTMLInputElement).value = value == null ? "" : String(value);
  else if (PROPERTIES.has(name)) (element as unknown as Record<string, Primitive>)[name] = value;
  // The page's policy refuses style attributes; the CSSOM is allowed.
  else if (name === "style")
    (element as HTMLElement).style.cssText = value == null || value === false ? "" : String(value);
  else if (value === false || value == null) element.removeAttribute(name);
  else element.setAttribute(name, value === true ? "" : String(value));
}

/** Appends `child` to `parent`; a function child becomes a region that rebuilds when what it read changes. */
export function insert(parent: Node, child: Child): void {
  if (child == null || typeof child === "boolean") return;
  if (typeof child === "function") return region(parent, child);
  if (Array.isArray(child)) for (const each of child) insert(parent, each);
  else parent.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
}

// The region's effect owns what it builds, so a rerun disposes the previous content's bindings. Its content is
// everything between its two markers, including what nested regions add later.
function region(parent: Node, fn: () => Child): void {
  const start = parent.appendChild(document.createComment(""));
  const end = parent.appendChild(document.createComment(""));
  effect(() => {
    const value = fn();
    const only = start.nextSibling;
    if ((typeof value === "string" || typeof value === "number") && only instanceof Text && only.nextSibling === end) {
      only.data = String(value);
      return;
    }
    clear(start, end);
    const fragment = document.createDocumentFragment();
    untrack(() => insert(fragment, value));
    end.before(fragment);
  });
}

/** Removes the nodes between two markers. */
export function clear(start: Node, end: Node): void {
  while (start.nextSibling && start.nextSibling !== end) start.nextSibling.remove();
}

export namespace JSX {
  export type Element = Node;
  export type IntrinsicElements = {
    [K in keyof HTMLElementTagNameMap]: Props<HTMLElementTagNameMap[K], HTMLElementEventMap>;
  } & {
    [K in Exclude<keyof SVGElementTagNameMap, keyof HTMLElementTagNameMap>]: Props<
      SVGElementTagNameMap[K],
      SVGElementEventMap
    >;
  };
  export interface ElementChildrenAttribute {
    children: {};
  }
}
