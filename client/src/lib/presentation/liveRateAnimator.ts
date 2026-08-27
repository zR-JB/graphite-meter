// It accepts already-selected visual targets and never retains measurement observations or publishes results.

const LIVE_RATE_SMOOTH_TAU_MS = 100;
export const LIVE_RATE_STALE_DELAY_MS = 250;
export const LIVE_RATE_DECAY_HORIZON_MS = 5_000;

export interface LiveRateValues {
  transfer: number;
  down: number;
  up: number;
}

interface LiveRateTarget {
  key: keyof LiveRateValues;
  target: number;
  /** Advances for every raw visual target observation, even at the same rate. */
  revision: number;
  context: string;
  active: boolean;
}

interface LiveRateState {
  context: string;
  value: number;
  lastRaw: number;
  revision: number;
  lastRawTargetAt: number;
  lastStepAt: number;
}

interface LiveRateFrame {
  value: number;
  active: boolean;
}

export class LiveRateAnimator {
  #states = new Map<keyof LiveRateValues, LiveRateState>();

  reset(): void {
    this.#states.clear();
  }

  step(
    input: LiveRateTarget,
    now: number,
    reducedMotion: boolean,
  ): LiveRateFrame {
    const target = Math.max(
      0,
      Number.isFinite(input.target) ? input.target : 0,
    );
    if (!input.active) {
      this.#states.delete(input.key);
      return { value: target, active: false };
    }

    let state = this.#states.get(input.key);
    if (!state || state.context !== input.context) {
      state = {
        context: input.context,
        value: target,
        lastRaw: target,
        revision: input.revision,
        lastRawTargetAt: now,
        lastStepAt: now,
      };
      this.#states.set(input.key, state);
      return { value: target, active: false };
    }

    if (state.revision !== input.revision || state.lastRaw !== target) {
      state.lastRaw = target;
      state.revision = input.revision;
      state.lastRawTargetAt = now;
    }

    const staleMs = Math.max(0, now - state.lastRawTargetAt);
    const decayAge = Math.max(0, staleMs - LIVE_RATE_STALE_DELAY_MS);
    const progress = Math.min(1, decayAge / LIVE_RATE_DECAY_HORIZON_MS);
    const effectiveTarget =
      state.lastRaw * Math.max(0, 1 - progress * progress);
    const elapsedMs = Math.max(0, now - state.lastStepAt);
    state.lastStepAt = now;
    if (reducedMotion) {
      state.value = effectiveTarget;
      return { value: state.value, active: false };
    }
    const alpha = 1 - Math.exp(-elapsedMs / LIVE_RATE_SMOOTH_TAU_MS);
    state.value += (effectiveTarget - state.value) * alpha;
    const moving = Math.abs(effectiveTarget - state.value) > 0.01;
    const decaying =
      decayAge > 0 &&
      decayAge < LIVE_RATE_DECAY_HORIZON_MS &&
      state.value > 0.01;
    return { value: state.value, active: moving || decaying };
  }
}
