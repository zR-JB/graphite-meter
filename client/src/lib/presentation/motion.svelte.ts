// One frame clock moves every animated value; it runs only while something moves.
import { prefersReducedMotion } from "svelte/motion";

type Task = (now: number) => boolean;
const tasks = new Set<Task>();
let frame = 0;

/** Reduced motion: every value still updates, in one frame rather than an animation. */
export const still = () => prefersReducedMotion.current;

function request(): void {
  if (!frame && tasks.size)
    frame = globalThis.requestAnimationFrame?.(run) ?? 0;
}

function run(now: number): void {
  frame = 0;
  for (const task of tasks) if (!task(now) || still()) tasks.delete(task);
  request();
}

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
