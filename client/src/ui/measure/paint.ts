// The instrument's drawing helpers on the shared frame clock.
import { animate, pageMs } from "../../lib/frame";
import { SVG_NS } from "../../lib/jsx-runtime";
import { batch, effect, onCleanup, signal, untrack, type Read } from "../../lib/signal";

/** A live view redraws at most this often: past 100 frames a second a needle, a plot or a digit looks no smoother. */
const LIVE_FRAME_MS = 10;
const BEAT_GAP_MS = 250;

let liveAt = Number.NEGATIVE_INFINITY;
let beatAt = Number.NEGATIVE_INFINITY;
let beatSeen: number | null = null;
let beaten = Number.NEGATIVE_INFINITY;
let onBeat = true;
const liveFrame = (now: number) => {
  if (now !== liveAt && now >= liveAt && now - liveAt < LIVE_FRAME_MS) return false;
  liveAt = now;
  return true;
};

/** Whether a frame draws views on `beat`, decided once a frame so far-apart views change together: after it moves, else every 250 ms. */
export function beatFrame(now: number, beat: number | null): boolean {
  if (beat === null) return true;
  if (now !== beatAt) {
    beatAt = now;
    onBeat = beat !== beatSeen || now < beaten || now - beaten >= BEAT_GAP_MS;
    if (onBeat) [beatSeen, beaten] = [beat, now];
  }
  return onBeat;
}

/** A view's painter: `wake()` draws next frame and on while `draw` returns true, on `beat`'s frames if given; `wake(true)` draws before the next layout. */
export function painter(
  draw: (now: number) => boolean,
  away?: Read<boolean>,
  beat?: () => number | null,
): (resized?: boolean) => void {
  let stop: (() => void) | null = null;
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
      if (!liveFrame(now) || (beat && !beatFrame(now, beat()) && !due)) return true;
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
    // After the callback or batch that resized the view, so every wake it caused is drawn in one go.
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

type Size = { width: number; height: number };

const measured = new Map<Element, { size: Size; follow: Set<(size: Size) => void> }>();
let sizer: ResizeObserver | null = null;

/** Each element's size in whole CSS px, so sub-pixel steps draw nothing; one shared observer batches a frame's resizes. */
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

/** A plot answers a mouse at once; a finger reads on a tap or sideways drag, and a vertical swipe scrolls past. */
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

/** A mono figure that changes every frame, in a contained box as wide as its longest text, so a write lays out nothing. */
export class Figure {
  readonly box: HTMLElement;
  readonly #text: Text;
  #chars: number;

  constructor(className: string, chars = 1) {
    this.box = document.createElement("span");
    this.box.className = `figure ${className}`;
    this.#text = this.box
      .appendChild(document.createElement("span"))
      .appendChild(document.createTextNode(""));
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

export function setText(node: Text, text: string): void {
  if (node.data !== text) node.data = text;
}

export const svg = <K extends keyof SVGElementTagNameMap>(
  tag: K,
  className?: string,
): SVGElementTagNameMap[K] => {
  const element = document.createElementNS(SVG_NS, tag);
  if (className) element.setAttribute("class", className);
  return element;
};
