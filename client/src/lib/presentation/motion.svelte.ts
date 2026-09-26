// One frame clock moves every animated value; it runs only while something moves.
import { untrack } from "svelte";
import { prefersReducedMotion } from "svelte/motion";

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
  for (const task of tasks) if (!task(now) || still()) tasks.delete(task);
  request();
}

/** The latest frame's time, for a redraw that cannot wait for the next frame. */
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

const GLIDE_MIN_MS = 50;
const GLIDE_MAX_MS = 600;
const SAMPLE_GAP_MS = 2_000;

interface Correction {
  /** Units per ms after this sample, so a clock keeps moving between samples. */
  rate?: number;
  max?: number;
  /** A fixed glide instead of the interval between samples. */
  over?: number;
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
    const snap = correction.snap || still() || !Number.isFinite(gap);
    this.#from = snap ? value : this.at(now);
    if (correction.over !== undefined) this.#glide = correction.over;
    else if (gap < SAMPLE_GAP_MS)
      this.#glide = Math.min(GLIDE_MAX_MS, Math.max(GLIDE_MIN_MS, gap));
    this.#to = value;
    this.#rate = rate;
    this.#max = max;
    this.#at = now;
    this.current = this.at(now);
    if (this.#from !== value || rate) this.#stop ??= animate(this.#frame);
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
    this.current = this.at(now);
    const moving =
      now - this.#at < this.#glide ||
      (this.#rate > 0 && this.current < this.#max);
    if (!moving) this.#stop = null;
    return moving;
  };
}

const HANDOFF_OUT_MS = 90;
const HANDOFF_IN_MS = 180;

/**
 * A view of a changing state: while its key holds, `shown` follows the value; a new key fades the old
 * view out, swaps and fades the new one in. A key that lasts less than the fade-out is never shown.
 */
export class Handoff<T> {
  shown: T = $state.raw() as T;
  opacity = $state(1);
  #key: unknown;
  #latest: T;
  #keyOf: (value: T) => unknown;
  #leaving = false;
  #from = 1;
  #at = 0;
  #stop: (() => void) | null = null;

  constructor(value: T, keyOf: (value: T) => unknown = (value) => value) {
    this.shown = this.#latest = value;
    this.#keyOf = keyOf;
    this.#key = keyOf(value);
  }

  set(value: T): void {
    this.#latest = value;
    const same = this.#keyOf(value) === this.#key;
    if (same && !this.#leaving) {
      this.shown = value;
      return;
    }
    if (still() || globalThis.document?.hidden) {
      this.#swap();
      this.opacity = 1;
      this.#stop?.();
      this.#stop = null;
      return;
    }
    // Turning back mid-fade continues from the opacity reached.
    this.#leaving = !same;
    if (same) this.shown = value;
    this.#from = this.opacity;
    this.#at = performance.now();
    this.#stop ??= animate(this.#frame);
  }

  #swap(): void {
    this.shown = this.#latest;
    this.#key = this.#keyOf(this.#latest);
    this.#leaving = false;
  }

  #frame = (now: number): boolean => {
    const since = Math.max(0, now - this.#at);
    if (this.#leaving) {
      this.opacity = Math.max(0, this.#from - since / HANDOFF_OUT_MS);
      if (this.opacity > 0) return true;
      this.#swap();
      this.#from = 0;
      this.#at = now;
      return true;
    }
    const t = Math.min(1, this.#from + since / HANDOFF_IN_MS);
    this.opacity = 1 - (1 - t) ** 2;
    if (t < 1) return true;
    this.#stop = null;
    return false;
  };
}

/** A Handoff that follows `get`, for the component being initialised. */
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
