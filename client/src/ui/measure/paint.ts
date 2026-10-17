// Holds the instrument's drawing helpers, a view's painter on the shared frame clock, whether an element is on screen
// in a visible page, a box's measured size, a plot's scrubbing, fixed-size figures and SVG elements.
import { animate, pageMs } from "../../lib/frame";
import { batch, effect, onCleanup, signal, untrack, type Read } from "../../lib/signal";

/** A live view redraws at most this often; past 100 frames a second a needle, a plot or a digit looks no smoother. */
const LIVE_FRAME_MS = 10;

/** Makes a view's painter. `wake()` draws on the next frame, and on every frame after while `draw` returns true,
    skipping frames closer than LIVE_FRAME_MS to the last it drew. `wake(true)`, for a change of size, draws instead
    before the frame's next layout, so a view keeps step with its box at any display rate. A change is drawn once
    however many wakes ask for it. While `away` holds it draws nothing, and it draws again as its view returns. It is
    released with its owner. */
export function painter(draw: (now: number) => boolean, away?: Read<boolean>): (resized?: boolean) => void {
  let stop: (() => void) | null = null;
  let drawn = Number.NEGATIVE_INFINITY;
  // A change waits to be drawn; the last draw is still moving; a draw is queued before the next layout.
  let due = false;
  let moving = false;
  let queued = false;
  let disposed = false;
  const halt = () => {
    stop?.();
    stop = null;
  };
  const paint = (now: number) => {
    drawn = now;
    due = false;
    moving = untrack(() => draw(now));
    return moving;
  };
  const start = () => {
    stop ??= animate((now) => {
      if (!due && !moving) {
        stop = null;
        return false;
      }
      if (now - drawn < LIVE_FRAME_MS) return true;
      if (paint(now)) return true;
      stop = null;
      return false;
    });
  };
  const wake = (resized = false) => {
    if (disposed || away?.peek()) return;
    due = true;
    if (!resized) return start();
    if (queued) return;
    queued = true;
    // It runs after the callback or batch that resized the view, so every wake it caused is drawn in one go.
    queueMicrotask(() => {
      queued = false;
      if (!disposed && due && !away?.peek() && paint(pageMs())) start();
    });
  };
  onCleanup(() => {
    disposed = true;
    halt();
  });
  if (!away) return wake;
  let gone = false;
  effect(() => {
    if (away()) {
      gone = true;
      halt();
    } else if (gone) {
      gone = false;
      due = true;
      start();
    }
  });
  return wake;
}

/** Says whether `element` is on screen in a visible page; `initial` until the first observation. */
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

type Size = { width: number; height: number };

/** Maps every measured element to its last size and the views that follow it. */
const measured = new Map<Element, { size: Size; follow: Set<(size: Size) => void> }>();
let sizer: ResizeObserver | null = null;

/** Gives each element's border-box size in whole CSS px, as `clientWidth` reads it, so the sub-pixel steps at a
    slide's end draw nothing until they add up to a pixel. One observer serves every view, so a frame's resizes arrive
    in one callback and one batch; sizes are read there, never by forcing layout. */
export function measure<K extends string>(elements: Record<K, Element>): Record<K, Read<Size>> {
  sizer ??= new ResizeObserver((records) =>
    batch(() => {
      for (const record of records) {
        const entry = measured.get(record.target);
        if (!entry) continue;
        const box = record.borderBoxSize[0];
        const width = Math.round(box?.inlineSize ?? record.contentRect.width);
        const height = Math.round(box?.blockSize ?? record.contentRect.height);
        if (entry.size.width === width && entry.size.height === height) continue;
        entry.size = { width, height };
        for (const follow of entry.follow) follow(entry.size);
      }
    }),
  );
  const sizes = {} as Record<K, Read<Size>>;
  for (const key in elements) {
    const element = elements[key];
    let entry = measured.get(element);
    if (!entry) {
      entry = { size: { width: 0, height: 0 }, follow: new Set() };
      measured.set(element, entry);
      sizer.observe(element);
    }
    const size = signal(entry.size);
    const follow = (next: Size) => size.set(next);
    const { follow: views } = entry;
    views.add(follow);
    onCleanup(() => {
      views.delete(follow);
      if (views.size) return;
      measured.delete(element);
      sizer!.unobserve(element);
    });
    sizes[key] = size;
  }
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

/** Holds a figure that changes every frame or every sample in a box of fixed size (`.figure`), as wide in ch as its
    longest text so far. Its text lays out alone in an absolutely placed, contained box inside it, so writing a new
    value lays out nothing around it. Mono figures only. */
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
