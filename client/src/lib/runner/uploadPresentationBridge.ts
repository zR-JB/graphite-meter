/** Presentation-only policy for an irregular authoritative upload feed. It has
 * no access to accumulators, charts, control buckets, or result reduction. */
export const UPLOAD_PRESENTATION_HINT_MAX_AGE_MS = 250;
/** A worker targets 500 ms POSTs. Keep each independently observed lane long
 * enough to form one aggregate, but never indefinitely retain a dead lane. */
export const UPLOAD_PRESENTATION_LANE_MAX_AGE_MS = 750;
/** Active fallback converges back to authority instead of snapping off when a
 * valid activation hint ages out between POST completions. */
export const UPLOAD_PRESENTATION_SETTLE_MS = 500;
export const UPLOAD_PRESENTATION_SETTLE_CADENCE_MS = 50;

interface LaneHint {
  rate: number;
  at: number;
}

interface ActiveHint {
  rate: number;
  lastFreshAt: number;
}

export class UploadPresentationBridge {
  #arrivals: number[] = [];
  #lastRate = 0;
  #lanes = new Map<number, LaneHint>();
  #active: ActiveHint | null = null;

  authoritative(rate: number, advancing: boolean, now: number): void {
    if (!advancing || rate <= 0) return;
    this.#arrivals.push(now);
    if (this.#arrivals.length > 5) this.#arrivals.shift();
    this.#lastRate = rate;
    // An advancing server checkpoint is immediately authoritative again. It
    // also invalidates local work that could belong to the previous gap.
    this.#lanes.clear();
    this.#active = null;
  }

  /** Record one locally timed POST completion by lane. This is deliberately
   * not byte accounting: it only supplies a candidate visual target. */
  hint(lane: number, bytes: number, elapsedMs: number, now: number): void {
    if (lane < 0 || bytes <= 0 || elapsedMs <= 0) return;
    this.#lanes.set(lane, {
      rate: bytes / (elapsedMs / 1_000),
      at: now,
    });
  }

  target(now: number, healthy: boolean, expectedLanes: number): number | null {
    if (!healthy || expectedLanes <= 0) {
      this.stop();
      return null;
    }
    const aggregate = this.#aggregateFreshHints(now, expectedLanes);
    if (this.#canActivate(now, aggregate)) {
      this.#active = {
        rate: this.#bounded(aggregate.rate),
        lastFreshAt: aggregate.latestAt,
      };
    }

    const active = this.#active;
    if (!active) return null;
    const afterFreshness = Math.max(
      0,
      now - active.lastFreshAt - UPLOAD_PRESENTATION_HINT_MAX_AGE_MS,
    );
    if (afterFreshness >= UPLOAD_PRESENTATION_SETTLE_MS) {
      this.#active = null;
      return null;
    }
    const progress = afterFreshness / UPLOAD_PRESENTATION_SETTLE_MS;
    // Smoothstep has no slope discontinuity at either end and cannot overshoot
    // the bounded local target or the current authoritative target.
    const eased = progress * progress * (3 - 2 * progress);
    return active.rate + (this.#lastRate - active.rate) * eased;
  }

  /** The runner wakes only while the bridge has a visual target to settle. */
  nextWakeMs(
    now: number,
    healthy: boolean,
    expectedLanes: number,
  ): number | null {
    return this.target(now, healthy, expectedLanes) === null
      ? null
      : UPLOAD_PRESENTATION_SETTLE_CADENCE_MS;
  }

  stop(): void {
    this.#arrivals = [];
    this.#lastRate = 0;
    this.#lanes.clear();
    this.#active = null;
  }

  #aggregateFreshHints(
    now: number,
    expectedLanes: number,
  ): { rate: number; latestAt: number } | null {
    let rate = 0;
    let latestAt = -Infinity;
    let count = 0;
    for (const [lane, hint] of this.#lanes) {
      if (now - hint.at > UPLOAD_PRESENTATION_LANE_MAX_AGE_MS) {
        this.#lanes.delete(lane);
        continue;
      }
      rate += hint.rate;
      latestAt = Math.max(latestAt, hint.at);
      count++;
    }
    // Do not multiply one lane by the configured count: uneven backpressure is
    // exactly why every expected lane must provide an independent observation.
    return count === expectedLanes && rate > 0 ? { rate, latestAt } : null;
  }

  #canActivate(
    now: number,
    aggregate: { rate: number; latestAt: number } | null,
  ): aggregate is { rate: number; latestAt: number } {
    if (
      !aggregate ||
      now - aggregate.latestAt > UPLOAD_PRESENTATION_HINT_MAX_AGE_MS ||
      this.#lastRate <= 0 ||
      this.#arrivals.length < 4
    )
      return false;
    const gaps = this.#arrivals.slice(1).map((t, i) => t - this.#arrivals[i]);
    const ordered = [...gaps].sort((a, b) => a - b);
    const median = ordered[Math.floor(ordered.length / 2)] ?? 0;
    const last = this.#arrivals.at(-1) ?? now;
    return now - last > Math.max(300, 3 * median);
  }

  #bounded(rate: number): number {
    return Math.max(
      this.#lastRate * 0.75,
      Math.min(this.#lastRate * 1.25, rate),
    );
  }
}
