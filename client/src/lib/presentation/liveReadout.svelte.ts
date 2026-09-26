// Live rates on the frame clock: they hold through a stage change, snap to its first evidence and fade a stall.
import type { LiveSample } from "../runner/contract";
import { Smoothed } from "./motion.svelte";

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

export class LiveReadout {
  readonly down = new Smoothed();
  readonly up = new Smoothed();
  readonly rtt = new Smoothed();
  /** False until the run's first evidence. */
  shown = $state(false);
  #run = -1;
  #phase = "";
  #stalled = false;

  get rates(): RatePair | null {
    return this.shown ? { down: this.down.current, up: this.up.current } : null;
  }

  /** Each sample corrects the readout; a sample without evidence holds it. */
  update(live: LiveSample | null, run: number, now?: number): void {
    if (run !== this.#run) {
      this.#run = run;
      this.#phase = "";
      this.shown = false;
    }
    const target = liveTargets(live);
    if (!target || !live || (live.stalled && this.#stalled)) return;
    const snap = !this.shown || live.phase !== this.#phase;
    const over = live.stalled ? STALL_FADE_MS : undefined;
    this.#stalled = live.stalled;
    this.#phase = live.phase;
    this.shown = true;
    this.down.set(target.down, { snap, over, now });
    this.up.set(target.up, { snap, over, now });
  }
}
