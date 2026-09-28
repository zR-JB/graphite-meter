// Live values on the frame clock: one stage's evidence at a time, fading a stall.
import type { LiveSample } from "../runner/contract";
import { Smoothed } from "./motion.svelte";

export const STALL_FADE_MS = 800;

interface RatePair {
  down: number;
  up: number;
}

/** The unanimated targets; null before the stage's first evidence. */
export function liveTargets(live: LiveSample | null): RatePair | null {
  if (live?.down == null && live?.up == null) return null;
  return { down: live.down ?? 0, up: live.bridgedUp ?? live.up ?? 0 };
}

export class LiveReadout {
  readonly down = new Smoothed();
  readonly up = new Smoothed();
  readonly rtt = new Smoothed();
  /** The stage the rates measure; null until that stage's first evidence. */
  phase = $state<LiveSample["phase"] | null>(null);
  #run = -1;
  #stalled = false;

  get rates(): RatePair | null {
    return this.phase ? { down: this.down.current, up: this.up.current } : null;
  }

  /** Each sample corrects the readout; a sample without evidence holds it, and a new stage starts empty. */
  update(live: LiveSample | null, run: number, now?: number): void {
    if (run !== this.#run || live?.phase !== this.phase) {
      this.#run = run;
      this.phase = null;
      this.#stalled = false;
    }
    const target = liveTargets(live);
    if (!target || !live || (live.stalled && this.#stalled)) return;
    const correction = {
      snap: !this.phase,
      over: live.stalled ? STALL_FADE_MS : undefined,
      now,
    };
    this.#stalled = live.stalled;
    this.phase = live.phase;
    this.down.set(target.down, correction);
    this.up.set(target.up, correction);
  }
}
