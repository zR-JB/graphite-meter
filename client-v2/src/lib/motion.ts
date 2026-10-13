// Values that move on the frame clock: glides that correct a value towards each sample, clocks that run at a rate,
// hand-offs between keyed views, and the damped spring every layout move uses. Reduced motion snaps them all.
import { animate, pageMs } from "./frame";
import { batch, effect, onCleanup, signal, untrack, type Read, type Signal } from "./signal";

const query = typeof matchMedia === "function" ? matchMedia("(prefers-reduced-motion: reduce)") : null;
const still = signal(query?.matches ?? false);
query?.addEventListener("change", () => still.set(query.matches));
/** Every value still updates, in one frame rather than an animation. */
export const reducedMotion: Read<boolean> = still;

const GLIDE_MIN_MS = 50;
const GLIDE_MAX_MS = 400;
/** After a pause this long the next sample reuses the previous glide rather than stretching over the pause. */
const SAMPLE_GAP_MS = 2_000;

export interface Sample {
  /** Units per ms the value keeps moving after the sample, stopping at `max`. */
  rate?: number;
  max?: number;
  /** A fixed glide over this many ms instead of the sample interval. */
  over?: number;
  snap?: boolean;
  now?: number;
}

/** A number a painter reads with `at(now)`, gliding to each sample over the interval between samples: samples
    correct the value, never step it. The offset from the sample fades linearly, so a clock keeps its own pace. */
export class Glide {
  #from: number;
  #to: number;
  #at = Number.NEGATIVE_INFINITY;
  #glide = 100;
  #rate = 0;
  #max = Number.POSITIVE_INFINITY;
  #fixed = false;

  constructor(value = 0) {
    this.#from = this.#to = value;
  }

  /** The last sample. */
  get target(): number {
    return this.#to;
  }

  at(t: number): number {
    const since = Math.max(0, t - this.#at);
    const u = Math.min(1, since / this.#glide);
    const truth = this.#rate ? Math.min(this.#max, this.#to + this.#rate * since) : this.#to;
    // A correction below what was shown never carries a clock past its bound.
    return Math.min(this.#max, truth + (this.#from - this.#to) * (1 - u));
  }

  /** A value that is not a finite number holds the last one. */
  set(value: number, sample: Sample = {}): void {
    const now = sample.now ?? pageMs();
    const rate = sample.rate ?? 0;
    if (!Number.isFinite(value) || !Number.isFinite(rate)) return;
    const max = rate ? (sample.max ?? Number.POSITIVE_INFINITY) : Number.POSITIVE_INFINITY;
    // A clock's sample past its bound is the bound.
    value = Math.min(value, max);
    const seen = this.at(now);
    const remaining = this.#fixed ? this.#at + this.#glide - now : 0;
    const snap = sample.snap || still.peek() || this.#at === Number.NEGATIVE_INFINITY;
    let glide = this.#glide;
    let fixed = false;
    if (sample.over !== undefined) [glide, fixed] = [sample.over, true];
    else if (remaining > 0 && !snap) [glide, fixed] = [remaining, true];
    else if (now - this.#at < SAMPLE_GAP_MS) glide = Math.min(GLIDE_MAX_MS, Math.max(GLIDE_MIN_MS, now - this.#at));
    this.#from = snap || !(glide > 0) ? value : seen;
    this.#to = value;
    this.#at = now;
    this.#glide = glide > 0 ? glide : 1;
    this.#rate = rate;
    this.#max = max;
    this.#fixed = fixed;
  }

  /** Stops where it is now. */
  hold(now = pageMs()): void {
    this.set(this.at(now), { snap: true, now });
  }
}

export const HANDOFF_OUT_MS = 90;

/** A keyed view: a new key fades the shown view out (`out`) and swaps it in once the fade-out ran; a key held
    shorter than that never shows; the same key updates in place; a view with nothing to fade out (`outMs` 0) swaps
    in the same frame. The fades themselves are CSS (.handoff). */
export class Handoff<T> {
  readonly #shown: Signal<T>;
  readonly #out = signal(false);
  #key: unknown;
  #latest: T;
  #stop: (() => void) | null = null;

  constructor(
    value: T,
    readonly keyOf: (value: T) => unknown,
    readonly outMs: number | ((leaving: T) => number) = HANDOFF_OUT_MS,
  ) {
    this.#shown = signal(value);
    this.#latest = value;
    this.#key = keyOf(value);
  }

  get shown(): Read<T> {
    return this.#shown;
  }

  get out(): Read<boolean> {
    return this.#out;
  }

  set(value: T): void {
    this.#latest = value;
    const hidden = typeof document !== "undefined" && document.hidden;
    if (this.keyOf(value) === this.#key || still.peek() || hidden) return this.#swap();
    const shown = this.#shown.peek();
    if ((typeof this.outMs === "function" ? this.outMs(shown) : this.outMs) <= 0) return this.#swap();
    this.#out.set(true);
    let start: number | null = null;
    this.#stop ??= animate((now) => {
      start ??= now;
      const shown = this.#shown.peek();
      if (now - start < (typeof this.outMs === "function" ? this.outMs(shown) : this.outMs) && !still.peek())
        return true;
      this.#stop = null;
      this.#swap();
      return false;
    });
  }

  #swap(): void {
    this.dispose();
    batch(() => {
      this.#key = this.keyOf(this.#latest);
      this.#shown.set(this.#latest);
      this.#out.set(false);
    });
  }

  dispose(): void {
    this.#stop?.();
    this.#stop = null;
  }
}

/** A hand-off that follows `get`, released with its owner. */
export function handoff<T>(get: () => T, keyOf: (value: T) => unknown, outMs?: Handoff<T>["outMs"]): Handoff<T> {
  const view = new Handoff(untrack(get), keyOf, outMs);
  effect(() => {
    const value = get();
    untrack(() => view.set(value));
  });
  onCleanup(() => view.dispose());
  return view;
}

const DAMPING = 0.81;
const OMEGA = 0.024;
const DECAY = DAMPING * OMEGA;
const RING = OMEGA * Math.sqrt(1 - DAMPING ** 2);
/** How long a spring move is drawn; it settles within 1 % in about 0.26 s and overshoots once by about 1.3 %. */
export const SPRING_MS = 420;

/** Offset and velocity (units per ms) `t` ms after a damped spring is released at `x0` moving at `v0`. */
export function spring([x0, v0]: readonly [number, number], t: number): [number, number] {
  const b = (v0 + DECAY * x0) / RING;
  const decay = Math.exp(-DECAY * t);
  const [cos, sin] = [Math.cos(RING * t), Math.sin(RING * t)];
  return [decay * (x0 * cos + b * sin), decay * (v0 * cos - (x0 * RING + DECAY * b) * sin)];
}
