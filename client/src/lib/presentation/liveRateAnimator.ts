// Interpolate formatted readouts; the runner and upload bridge own target freshness.
const LIVE_RATE_SMOOTH_TAU_MS = 100;

export interface LiveRateValues {
  transfer: number;
  down: number;
  up: number;
}

export class LiveRateAnimator {
  #context: string | null = null;
  #values: LiveRateValues = { transfer: 0, down: 0, up: 0 };
  #lastStepAt = 0;

  step(
    input: { values: LiveRateValues; context: string; active: boolean },
    now: number,
    reducedMotion: boolean,
  ): { values: LiveRateValues; active: boolean } {
    const snap =
      !input.active || this.#context !== input.context || reducedMotion;
    const alpha = snap
      ? 1
      : 1 -
        Math.exp(
          -Math.max(0, now - this.#lastStepAt) / LIVE_RATE_SMOOTH_TAU_MS,
        );
    this.#context = input.active ? input.context : null;
    this.#lastStepAt = now;
    let active = false;
    const interpolate = (key: keyof LiveRateValues) => {
      const target = Math.max(
        0,
        Number.isFinite(input.values[key]) ? input.values[key] : 0,
      );
      const value = this.#values[key] + (target - this.#values[key]) * alpha;
      if (Math.abs(target - value) <= 0.01) return target;
      active = true;
      return value;
    };
    this.#values = {
      transfer: interpolate("transfer"),
      down: interpolate("down"),
      up: interpolate("up"),
    };
    return { values: this.#values, active };
  }
}
