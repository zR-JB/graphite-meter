// Readouts ease toward the live rates, hold through a stage change and fade out a stall.
import type { LiveSample } from "../runner/contract";

const SMOOTH_TAU_MS = 100;
export const STALL_FADE_MS = 800;

export interface RatePair {
  down: number;
  up: number;
}

/** The unanimated targets; null before the stage's first evidence. */
export function liveTargets(live: LiveSample | null): RatePair | null {
  if (live?.down == null && live?.up == null) return null;
  return { down: live.down ?? 0, up: live.bridgedUp ?? live.up ?? 0 };
}

export class LiveRateAnimator {
  #run = -1;
  #phase = "";
  #values: RatePair | null = null;
  #stall: { at: number; from: RatePair } | null = null;
  #at = 0;

  /** Values are null until the run's first evidence; `active` asks for another frame. */
  step(
    live: LiveSample | null,
    run: number,
    now: number,
    reducedMotion: boolean,
  ): { values: RatePair | null; active: boolean } {
    const alpha = 1 - Math.exp(-Math.max(0, now - this.#at) / SMOOTH_TAU_MS);
    this.#at = now;
    if (run !== this.#run) {
      this.#run = run;
      this.#values = this.#stall = null;
    }
    const target = liveTargets(live);
    const previous = this.#values;
    if (!target || !live) return { values: previous, active: false };
    if (live.stalled && previous) {
      this.#stall ??= { at: now, from: previous };
      const elapsed = reducedMotion ? STALL_FADE_MS : now - this.#stall.at;
      const k = Math.max(0, 1 - elapsed / STALL_FADE_MS);
      const { from } = this.#stall;
      this.#values = { down: from.down * k, up: from.up * k };
      return { values: this.#values, active: k > 0 };
    }
    this.#stall = null;
    // A new stage's first evidence, like reduced motion, snaps.
    const from =
      previous && live.phase === this.#phase && !reducedMotion
        ? previous
        : target;
    this.#phase = live.phase;
    const ease = (key: keyof RatePair) => {
      const value = from[key] + (target[key] - from[key]) * alpha;
      return Math.abs(target[key] - value) <= 0.01 ? target[key] : value;
    };
    this.#values = { down: ease("down"), up: ease("up") };
    const active =
      this.#values.down !== target.down || this.#values.up !== target.up;
    return { values: this.#values, active };
  }
}
