// One frame clock moves every animated value; it runs only while something moves.
import { flushSync, untrack } from "svelte";
import { expoOut } from "svelte/easing";
import { prefersReducedMotion } from "svelte/motion";
import type { TransitionConfig } from "svelte/transition";

type Task = (now: number) => boolean;
const tasks = new Set<Task>();
let frame = 0;
let watching = false;

function visibilityChanged(): void {
  if (globalThis.document?.hidden) {
    if (frame) globalThis.cancelAnimationFrame?.(frame);
    frame = 0;
  } else request();
}

function releaseClock(): void {
  if (tasks.size) return;
  if (frame) globalThis.cancelAnimationFrame?.(frame);
  frame = 0;
  if (watching)
    document.removeEventListener("visibilitychange", visibilityChanged);
  watching = false;
}

/** Reduced motion: every value still updates, in one frame rather than an animation. */
export const still = () => prefersReducedMotion.current;

const FLIP_MS = 380;
const FLIP_EASE = "cubic-bezier(0.22, 1.2, 0.36, 1)";
// Neighbours have moved most of the way by then: an arriving element appears into room already made.
const ROOM_MS = 140;
// A sheet moves without overshoot: it is a surface sliding to its edge, not a part settling into place.
const SHEET_EASE = "cubic-bezier(0.16, 1, 0.3, 1)";
// As `--dur-sheet`, which keeps a closing sheet displayed while it leaves.
const SHEET_MS = 320;
let flipping = false;
/** The rendered elements marked `data-flip`, by key; an element that is not displayed has no place. */
const boxes = (only?: readonly string[]) =>
  new Map(
    Array.from(document.querySelectorAll<HTMLElement>("[data-flip]"))
      .filter((el) => !only || only.includes(el.dataset.flip!))
      .map((el) => ({ el, box: el.getBoundingClientRect() }))
      .filter(({ box }) => box.width > 0 || box.height > 0)
      .map((entry) => [entry.el.dataset.flip!, entry]),
  );

/** A change that reshapes the console applies at once, in the frame of the click that asked for it. Then every
    element marked `data-flip` that stayed glides from its old place to its new one on the compositor; one also
    marked `data-flip-resize` changes width on the same curve, a layout per frame within it. Two
    things never share a place: a leaving one goes at once and its neighbours close over it, and an arriving one
    waits for its neighbours to make room, then rises in. An element with `data-flip-edge` (`left` or `right`)
    is a sheet: it slides in from beyond that edge and back out, without overshoot. `only`
    names the keys that move, so a change can move a surface as one rather than each part on its own. A flip
    within a flip is part of the outer one. */
export function flip(update: () => void, only?: readonly string[]): void {
  if (flipping || still() || globalThis.document?.hidden !== false) {
    update();
    return;
  }
  const before = boxes(only);
  flipping = true;
  try {
    update();
    flushSync();
  } finally {
    flipping = false;
  }
  const after = boxes(only);
  // With a sheet in it, everything that moves moves as one surface with the sheet.
  const sheet = [...before.values(), ...after.values()].some(
    ({ el }) => el.dataset.flipEdge,
  );
  const moveMs = sheet ? SHEET_MS : FLIP_MS;
  const moveEase = sheet ? SHEET_EASE : FLIP_EASE;
  const arriving: HTMLElement[] = [];
  let moved = false;
  for (const [key, { el, box }] of after) {
    const old = before.get(key);
    const edge = el.dataset.flipEdge;
    if (!old && edge) {
      const off = edge === "left" ? -box.right : innerWidth - box.left;
      el.animate([{ translate: `${off}px 0` }, { translate: "0 0" }], {
        duration: SHEET_MS,
        easing: SHEET_EASE,
      });
      continue;
    }
    if (!old) {
      arriving.push(el);
      continue;
    }
    const dx = old.box.left - box.left;
    const dy = old.box.top - box.top;
    if (Math.abs(dx) >= 0.5 || Math.abs(dy) >= 0.5) {
      moved = true;
      el.animate([{ translate: `${dx}px ${dy}px` }, { translate: "0 0" }], {
        duration: moveMs,
        easing: moveEase,
      });
    }
    // An element marked `data-flip-resize` changes width on the move's curve, so neighbours tile in every frame;
    // any other wider element opens from its old width rather than jumping to the new one.
    const grew = box.width - old.box.width;
    if (Math.abs(grew) >= 1 && el.dataset.flipResize !== undefined) {
      moved = true;
      el.animate(
        [{ width: `${old.box.width}px` }, { width: `${box.width}px` }],
        { duration: moveMs, easing: moveEase },
      );
    } else if (grew >= 1) {
      moved = true;
      el.animate(
        [
          { clipPath: `inset(0 ${grew}px 0 0 round var(--r-surface))` },
          { clipPath: "inset(0 0 0 0 round var(--r-surface))" },
        ],
        { duration: moveMs, easing: SHEET_EASE },
      );
    }
  }
  // An arriving element waits only while neighbours move to make its room.
  for (const el of arriving)
    el.animate(
      [
        { opacity: 0, translate: "0 8px" },
        { opacity: 1, translate: "0 0" },
      ],
      {
        duration: FLIP_MS - ROOM_MS,
        delay: moved ? ROOM_MS : 0,
        easing: FLIP_EASE,
        fill: "backwards",
      },
    );
  for (const [key, { el, box }] of before) {
    const edge = el.dataset.flipEdge;
    if (after.has(key) || !edge) continue;
    // A sheet no longer displayed leaves to its edge as a copy at its old place.
    const ghost = el.cloneNode(true) as HTMLElement;
    ghost.removeAttribute("data-flip");
    ghost.setAttribute("aria-hidden", "true");
    ghost.inert = true;
    Object.assign(ghost.style, {
      position: "fixed",
      left: `${box.left}px`,
      top: `${box.top}px`,
      width: `${box.width}px`,
      height: `${box.height}px`,
      margin: "0",
      pointerEvents: "none",
      zIndex: "1",
    });
    document.body.append(ghost);
    const off = edge === "left" ? -box.right : innerWidth - box.left;
    ghost
      .animate([{ translate: "0 0" }, { translate: `${off}px 0` }], {
        duration: SHEET_MS,
        easing: SHEET_EASE,
        fill: "forwards",
      })
      .finished.finally(() => ghost.remove());
  }
}

function request(): void {
  if (!frame && tasks.size && !globalThis.document?.hidden)
    frame = globalThis.requestAnimationFrame?.(run) ?? 0;
}

function run(now: number): void {
  frame = 0;
  for (const task of tasks) if (!task(now)) tasks.delete(task);
  releaseClock();
  request();
}

/** Runs `task` on every frame until it returns false or the returned stop is called. */
export function animate(task: Task): () => void {
  tasks.add(task);
  if (!watching && globalThis.document) {
    document.addEventListener("visibilitychange", visibilityChanged);
    watching = true;
  }
  request();
  return () => {
    tasks.delete(task);
    releaseClock();
  };
}

/** Wall time for labels such as "5 min ago"; frames use the frame clock. */
export const wallNow = () => Date.now();

/** Runs `task` once on the next frame. */
export const nextFrame = (task: (now: number) => void): (() => void) =>
  animate((now) => {
    task(now);
    return false;
  });

/** Subscribers write the DOM on every change; below a millionth none can show (CSS keeps six digits). */
const unseen = (a: number, b: number) =>
  Math.abs(a - b) <= 1e-6 * Math.max(1, Math.abs(b));

const GLIDE_MIN_MS = 50;
// A live value redraws at most this often; past 100 frames a second a needle or a digit looks no smoother.
const LIVE_FRAME_MS = 10;
const GLIDE_MAX_MS = 400;
const SAMPLE_GAP_MS = 2_000;

interface Correction {
  /** Units per ms after this sample, so a clock keeps moving between samples. */
  rate?: number;
  max?: number;
  /** A fixed glide instead of the interval between samples; later samples retarget within it at its pace. */
  over?: number;
  /** A fixed glide eases out (cubic) instead of keeping a constant pace: a move the user watches start and land. */
  ease?: boolean;
  /** Once no fixed glide is under way, snap instead of gliding over the sample interval. */
  finish?: boolean;
  snap?: boolean;
  now?: number;
}

/** Glides to each sample over the interval between samples: samples correct the value, never step it. */
export class Smoothed {
  current = $state(0);
  #from = 0;
  #to = 0;
  #rate = 0;
  #max = Infinity;
  #at = -Infinity;
  #glide = 100;
  #fixed = false;
  #ease = false;
  #shown = -Infinity;
  #stop: (() => void) | null = null;

  /** The last sample. */
  get target(): number {
    return this.#to;
  }

  /** The value at a frame time, for readers outside the reactive graph. */
  at(now: number): number {
    const since = Math.max(0, now - this.#at);
    const truth = this.#rate
      ? Math.min(this.#max, this.#to + this.#rate * since)
      : this.#to;
    // The offset from the sample fades linearly, so a clock keeps its own pace; an eased glide settles into place.
    const left = Math.max(0, 1 - since / this.#glide);
    const fade = this.#ease ? left ** 3 : left;
    return fade > 0 ? truth + (this.#from - this.#to) * fade : truth;
  }

  /** A value that is not a finite number holds the last one; the first sample snaps. */
  set(value: number, correction: Correction = {}): void {
    const { rate = 0, max = Infinity, now = performance.now() } = correction;
    if (!Number.isFinite(value) || !Number.isFinite(rate)) return;
    const gap = now - this.#at;
    const left = this.#at + this.#glide - now;
    const within = this.#fixed && left > 0 && correction.over === undefined;
    const snap =
      correction.snap ||
      still() ||
      !Number.isFinite(gap) ||
      (correction.finish && !within);
    this.#from = snap ? value : this.at(now);
    this.#fixed = correction.over !== undefined || (within && !snap);
    this.#ease =
      correction.over !== undefined ? !!correction.ease : within && this.#ease;
    if (correction.over !== undefined) this.#glide = correction.over;
    else if (within) this.#glide = left;
    else if (gap < SAMPLE_GAP_MS)
      this.#glide = Math.min(GLIDE_MAX_MS, Math.max(GLIDE_MIN_MS, gap));
    this.#to = value;
    this.#rate = rate;
    this.#max = max;
    this.#at = now;
    this.#publish(this.at(now));
    if (!unseen(this.#from, value) || (rate && this.current < max))
      this.#stop ??= animate(this.#frame);
    else this.dispose();
  }

  /** Releases the frame task when its view leaves; a later sample can start it again. */
  dispose(): void {
    this.#stop?.();
    this.#stop = null;
  }

  #publish(value: number): void {
    if (
      value === this.#to ? value !== this.current : !unseen(value, this.current)
    )
      this.current = value;
  }

  /** Stops where it is now. */
  hold(): void {
    this.set(this.at(performance.now()), { snap: true });
  }

  /** Publishes the current value without changing its course, for reduced motion. */
  sync(): void {
    const now = performance.now();
    this.set(this.at(now), {
      rate: this.#rate,
      max: this.#max,
      snap: true,
      now,
    });
  }

  #frame = (now: number): boolean => {
    if (still()) {
      const since = Math.max(0, now - this.#at);
      this.#publish(Math.min(this.#max, this.#to + this.#rate * since));
      this.#stop = null;
      return false;
    }
    const value = this.at(now);
    const moving =
      now - this.#at < this.#glide || (this.#rate > 0 && value < this.#max);
    if (!moving) this.#stop = null;
    // A fixed glide takes every display frame; a live value skips the frames closer than LIVE_FRAME_MS.
    if (!moving || this.#fixed || now - this.#shown >= LIVE_FRAME_MS) {
      this.#shown = now;
      this.#publish(value);
    }
    return moving;
  };
}

export const HANDOFF_OUT_MS = 90;

/** A new key fades the shown view out and the new one in; a key shorter than the fade-out never shows. The
    fades are the stylesheet's (`.handoff`, `.handoff-out`, on the compositor); this only flips `out` on the frame
    clock and swaps the view once the fade-out has run, `outMs` after it began; a view that leaves with a longer
    move of its own (`outMs` of the leaving view) keeps the stage that long. */
export class Handoff<T> {
  shown: T = $state.raw() as T;
  /** True while the shown view fades out before the next one takes its place. */
  out = $state(false);
  #key: unknown;
  #latest: T;
  #keyOf: (value: T) => unknown;
  #stop: (() => void) | null = null;
  #outAt = 0;
  #outMs: number | ((leaving: T) => number);

  constructor(
    value: T,
    keyOf: (value: T) => unknown = (value) => value,
    outMs: number | ((leaving: T) => number) = HANDOFF_OUT_MS,
  ) {
    this.#outMs = outMs;
    this.shown = this.#latest = value;
    this.#keyOf = keyOf;
    this.#key = keyOf(value);
  }

  set(value: T): void {
    this.#latest = value;
    if (this.#keyOf(value) === this.#key) {
      this.shown = value;
      if (this.#stop) this.#swap();
    } else if (still() || globalThis.document?.hidden) this.#swap();
    else {
      this.out = true;
      this.#stop ??= animate(this.#frame);
    }
  }

  #swap(): void {
    this.#stop?.();
    this.#stop = null;
    this.#outAt = 0;
    this.shown = this.#latest;
    this.#key = this.#keyOf(this.#latest);
    this.out = false;
  }

  #frame = (now: number): boolean => {
    this.#outAt ||= now;
    const outMs =
      typeof this.#outMs === "number" ? this.#outMs : this.#outMs(this.shown);
    if (now - this.#outAt < outMs && !still()) return true;
    this.#swap();
    return false;
  };

  dispose(): void {
    this.#stop?.();
    this.#stop = null;
  }
}

export function handoff<T>(
  get: () => T,
  keyOf?: (value: T) => unknown,
  outMs?: number | ((leaving: T) => number),
): Handoff<T> {
  const view = new Handoff(untrack(get), keyOf, outMs);
  $effect.pre(() => {
    const value = get();
    untrack(() => view.set(value));
  });
  $effect(() => () => view.dispose());
  return view;
}

const REVEAL_MS = 180;

/** A row that appears in a panel unfolds from its own height, row floor included, and fades in as it opens. */
export function reveal(node: Element): TransitionConfig {
  const style = getComputedStyle(node);
  const px = (value: string) => parseFloat(value) || 0;
  const height = px(style.height);
  const floor = px(style.minHeight);
  const [top, bottom] = [px(style.paddingTop), px(style.paddingBottom)];
  const edge = px(style.borderTopWidth);
  return {
    duration: still() ? 0 : REVEAL_MS,
    easing: expoOut,
    css: (t) =>
      `overflow: hidden; opacity: ${Math.min(1, t * 2)};` +
      `height: ${t * height}px; min-height: ${t * floor}px;` +
      `padding-block: ${t * top}px ${t * bottom}px; border-top-width: ${t * edge}px;`,
  };
}
