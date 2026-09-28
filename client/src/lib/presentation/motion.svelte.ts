// One frame clock moves every animated value; it runs only while something moves.
import { untrack } from "svelte";
import { expoOut } from "svelte/easing";
import { prefersReducedMotion } from "svelte/motion";
import type { TransitionConfig } from "svelte/transition";

type Task = (now: number) => boolean;
const tasks = new Set<Task>();
let frame = 0;
let lastFrame = 0;

/** Reduced motion: every value still updates, in one frame rather than an animation. */
export const still = () => prefersReducedMotion.current;

function request(): void {
  if (!frame && tasks.size)
    frame = globalThis.requestAnimationFrame?.(run) ?? 0;
}

function run(now: number): void {
  frame = 0;
  lastFrame = now;
  for (const task of tasks) if (!task(now)) tasks.delete(task);
  request();
}

/** The shared clock's latest frame time. */
export const frameTime = () => lastFrame;

/** Runs `task` on every frame until it returns false or the returned stop is called. */
export function animate(task: Task): () => void {
  tasks.add(task);
  request();
  return () => void tasks.delete(task);
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
const GLIDE_MAX_MS = 600;
const SAMPLE_GAP_MS = 2_000;

interface Correction {
  /** Units per ms after this sample, so a clock keeps moving between samples. */
  rate?: number;
  max?: number;
  /** A fixed glide instead of the interval between samples; later samples retarget within it at its pace. */
  over?: number;
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
    // The offset from the sample fades linearly, so a clock keeps its own pace.
    const fade = Math.max(0, 1 - since / this.#glide);
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
    if (correction.over !== undefined) this.#glide = correction.over;
    else if (within) this.#glide = left;
    else if (gap < SAMPLE_GAP_MS)
      this.#glide = Math.min(GLIDE_MAX_MS, Math.max(GLIDE_MIN_MS, gap));
    this.#to = value;
    this.#rate = rate;
    this.#max = max;
    this.#at = now;
    this.#publish(this.at(now));
    if (!unseen(this.#from, value) || rate) this.#stop ??= animate(this.#frame);
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
    this.#publish(value);
    const moving =
      now - this.#at < this.#glide || (this.#rate > 0 && value < this.#max);
    if (!moving) this.#stop = null;
    return moving;
  };
}

const HANDOFF_OUT_MS = 90;
const HANDOFF_IN_MS = 180;

/** A new key fades the shown view out and the new one in; a key shorter than the fade-out never shows. */
export class Handoff<T> {
  shown: T = $state.raw() as T;
  #fade = new Smoothed();
  #key: unknown;
  #latest: T;
  #keyOf: (value: T) => unknown;
  #stop: (() => void) | null = null;

  constructor(value: T, keyOf: (value: T) => unknown = (value) => value) {
    this.shown = this.#latest = value;
    this.#keyOf = keyOf;
    this.#key = keyOf(value);
    this.#fade.set(1, { snap: true });
  }

  get opacity(): number {
    return this.#fade.current;
  }

  set(value: T): void {
    this.#latest = value;
    if (this.#keyOf(value) === this.#key) {
      this.shown = value;
      if (this.#stop) this.#swap();
    } else if (still() || globalThis.document?.hidden) this.#swap();
    else {
      this.#fade.set(0, { over: HANDOFF_OUT_MS * this.#fade.current });
      this.#stop ??= animate(this.#frame);
    }
  }

  #swap(): void {
    this.#stop?.();
    this.#stop = null;
    this.shown = this.#latest;
    this.#key = this.#keyOf(this.#latest);
    this.#fade.set(1, { over: HANDOFF_IN_MS * (1 - this.#fade.current) });
  }

  #frame = (): boolean => {
    if (this.#fade.current > 0 && !still()) return true;
    this.#swap();
    return false;
  };
}

export function handoff<T>(
  get: () => T,
  keyOf?: (value: T) => unknown,
): Handoff<T> {
  const view = new Handoff(untrack(get), keyOf);
  $effect.pre(() => {
    const value = get();
    untrack(() => view.set(value));
  });
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
