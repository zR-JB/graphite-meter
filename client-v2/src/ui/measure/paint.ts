// The instrument's drawing helpers: a view's painter on the shared frame clock, whether an element is on screen in a
// visible page, a box's measured size, a plot's scrubbing, fixed-size figures and SVG elements.
import { animate } from "../../lib/frame";
import { batch, onCleanup, signal, type Read } from "../../lib/signal";

/** A view's painter: `wake()` draws from the next frame until `draw` returns false. Released with its owner. */
export function painter(draw: (now: number) => boolean): () => void {
  let stop: (() => void) | null = null;
  onCleanup(() => {
    stop?.();
    stop = null;
  });
  return () => {
    stop ??= animate((now) => {
      if (draw(now)) return true;
      stop = null;
      return false;
    });
  };
}

/** Whether `element` is on screen in a visible page; `initial` until the first observation. */
export function inView(element: Element, initial: boolean): Read<boolean> {
  const seen = signal(initial);
  let intersecting = initial;
  const update = () => seen.set(intersecting && !document.hidden);
  const observer = new IntersectionObserver((entries) => {
    intersecting = entries.at(-1)?.isIntersecting ?? intersecting;
    update();
  });
  observer.observe(element);
  document.addEventListener("visibilitychange", update);
  onCleanup(() => {
    observer.disconnect();
    document.removeEventListener("visibilitychange", update);
  });
  return seen;
}

/** Each element's border-box size in CSS px, from one observer; read in its callbacks, never by forcing layout. */
export function measure<K extends string>(
  elements: Record<K, Element>,
): Record<K, Read<{ width: number; height: number }>> {
  const sizes = {} as Record<K, ReturnType<typeof signal<{ width: number; height: number }>>>;
  const keys = new Map<Element, K>();
  for (const key in elements) {
    sizes[key] = signal({ width: 0, height: 0 });
    keys.set(elements[key], key);
  }
  const observer = new ResizeObserver((records) =>
    batch(() => {
      for (const record of records) {
        const box = record.borderBoxSize[0];
        const key = keys.get(record.target)!;
        const width = box?.inlineSize ?? record.contentRect.width;
        const height = box?.blockSize ?? record.contentRect.height;
        const previous = sizes[key].peek();
        if (previous.width !== width || previous.height !== height) sizes[key].set({ width, height });
      }
    }),
  );
  for (const element of keys.keys()) observer.observe(element);
  onCleanup(() => observer.disconnect());
  return sizes;
}

/** A plot answers a mouse at once; a finger reads it on a tap or a sideways drag, and a vertical swipe scrolls past.
    The plot sets `touch-action: pan-y pinch-zoom`. */
export function scrub(read: (event: PointerEvent) => void, clear: (event: PointerEvent) => void) {
  const DRAG_PX = 8;
  let touch: { id: number; x: number; y: number; dragging: boolean } | null = null;
  return {
    onpointerdown(event: PointerEvent) {
      if (event.pointerType !== "touch") return read(event);
      touch = { id: event.pointerId, x: event.clientX, y: event.clientY, dragging: false };
    },
    onpointermove(event: PointerEvent) {
      if (event.pointerType !== "touch") return read(event);
      if (touch?.id !== event.pointerId) return;
      const dx = Math.abs(event.clientX - touch.x);
      if (!touch.dragging && (dx < DRAG_PX || dx < Math.abs(event.clientY - touch.y))) return;
      touch.dragging = true;
      read(event);
    },
    // A tap keeps its reading until focus moves on; a drag reads only while the finger is down.
    onpointerup(event: PointerEvent) {
      if (touch?.id !== event.pointerId) return;
      if (touch.dragging) clear(event);
      else read(event);
      touch = null;
    },
    onpointercancel(event: PointerEvent) {
      if (touch?.id !== event.pointerId) return;
      touch = null;
      clear(event);
    },
  };
}

/** A figure that changes every frame or every sample: a box of fixed size (`.figure`), as wide in ch as its longest
    text so far, whose text lays out alone in an absolutely placed, contained box inside it, so writing a new value
    lays out nothing around it. Mono figures only. */
export class Figure {
  readonly box: HTMLElement;
  readonly #text: Text;
  #chars: number;

  constructor(className: string, chars = 1) {
    this.box = document.createElement("span");
    this.box.className = `figure ${className}`;
    this.#text = this.box.appendChild(document.createElement("span")).appendChild(document.createTextNode(""));
    this.#chars = chars;
    this.box.style.inlineSize = `${chars}ch`;
  }

  set(text: string): void {
    if (this.#text.data === text) return;
    this.#text.data = text;
    if (text.length > this.#chars) this.box.style.inlineSize = `${(this.#chars = text.length)}ch`;
  }
}

/** Writes an attribute only when its value changes, as each write restyles an SVG mark. */
export function setAttr(element: Element, name: string, value: string): void {
  if (element.getAttribute(name) !== value) element.setAttribute(name, value);
}

/** Writes a text node only when its text changes. */
export function setText(node: Text, text: string): void {
  if (node.data !== text) node.data = text;
}

export const svg = <K extends keyof SVGElementTagNameMap>(tag: K, className?: string): SVGElementTagNameMap[K] => {
  const element = document.createElementNS("http://www.w3.org/2000/svg", tag);
  if (className) element.setAttribute("class", className);
  return element;
};
