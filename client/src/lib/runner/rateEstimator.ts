/**
 * Canonical live-throughput presentation from exact byte/time observations.
 * The estimator owns presentation and regime detection only. Final reduction
 * and stability consume the same observations independently.
 */

export const PRESENTATION_MIN_WINDOW_MS = 800;
export const REGIME_FAST_WINDOW_MS = 750;
export const REGIME_BASELINE_READY_MS = 2_000;
export const REGIME_DOWNSHIFT_ENTER_RATIO = 0.75;
export const REGIME_DOWNSHIFT_CANCEL_RATIO = 0.85;
export const REGIME_DOWNSHIFT_CONFIRM_MS = 750;
export const REGIME_UPSHIFT_ENTER_RATIO = 1.2;
export const REGIME_UPSHIFT_CANCEL_RATIO = 1.1;
export const REGIME_UPSHIFT_CONFIRM_MS = 500;
export const STALL_PRESENTATION_MS = 800;

export interface RateObservation {
  bytes: number;
  durationMs: number;
}

export type RegimeCandidateKind = "down" | "up";

export interface RateEstimate {
  presentedBytesPerSec: number;
  fastBytesPerSec: number;
  evidenceMs: number;
  regimeAgeMs: number;
  regimeId: number;
  regimeChanged: boolean;
  candidate: RegimeCandidateKind | null;
}

interface PositionedObservation {
  start: number;
  end: number;
  bytes: number;
}

interface Candidate {
  kind: RegimeCandidateKind;
  start: number;
  reference: number;
}

function finiteNonNegative(value: number): number {
  return Number.isFinite(value) ? Math.max(0, value) : 0;
}

export function presentationWindowMs(regimeAgeMs: number): number {
  const age = finiteNonNegative(regimeAgeMs);
  return Math.min(age, Math.max(PRESENTATION_MIN_WINDOW_MS, age * 0.5));
}

/** Exact bytes/time for an interval, prorating an observation at its edges. */
function intervalRate(
  observations: PositionedObservation[],
  start: number,
  end: number,
): number {
  if (!(end > start)) return 0;
  let bytes = 0;
  let representedMs = 0;
  for (const observation of observations) {
    const overlapStart = Math.max(start, observation.start);
    const overlapEnd = Math.min(end, observation.end);
    const overlap = overlapEnd - overlapStart;
    if (overlap <= 0) continue;
    const duration = observation.end - observation.start;
    bytes += observation.bytes * (overlap / duration);
    representedMs += overlap;
  }
  return representedMs > 0 ? (bytes * 1_000) / representedMs : 0;
}

export class GrowingRateEstimator {
  #observations: PositionedObservation[] = [];
  #evidenceMs = 0;
  #regimeStartMs = 0;
  #regimeId = 0;
  #candidate: Candidate | null = null;
  #presentedBytesPerSec = 0;
  #fastBytesPerSec = 0;

  reset(): void {
    this.#observations = [];
    this.#evidenceMs = 0;
    this.#regimeStartMs = 0;
    this.#regimeId = 0;
    this.#candidate = null;
    this.#presentedBytesPerSec = 0;
    this.#fastBytesPerSec = 0;
  }

  /** End the current regime without inventing evidence. */
  invalidateRegime(): void {
    this.#candidate = null;
    this.#regimeStartMs = this.#evidenceMs;
    this.#regimeId++;
    this.#presentedBytesPerSec = 0;
    this.#fastBytesPerSec = 0;
    this.#prune();
  }

  observe(input: RateObservation): RateEstimate {
    const durationMs = finiteNonNegative(input.durationMs);
    if (durationMs <= 0) return this.snapshot(false);
    const start = this.#evidenceMs;
    const end = start + durationMs;
    this.#observations.push({
      start,
      end,
      bytes: finiteNonNegative(input.bytes),
    });
    this.#evidenceMs = end;

    this.#recalculateRates();
    const regimeChanged = this.#updateCandidate(start);
    if (regimeChanged) this.#recalculateRates();
    this.#prune();
    return this.snapshot(regimeChanged);
  }

  snapshot(regimeChanged = false): RateEstimate {
    return {
      presentedBytesPerSec: this.#presentedBytesPerSec,
      fastBytesPerSec: this.#fastBytesPerSec,
      evidenceMs: this.#evidenceMs,
      regimeAgeMs: this.#evidenceMs - this.#regimeStartMs,
      regimeId: this.#regimeId,
      regimeChanged,
      candidate: this.#candidate?.kind ?? null,
    };
  }

  static stallRate(fromBytesPerSec: number, elapsedMs: number): number {
    const fraction = Math.min(
      1,
      Math.max(0, elapsedMs / STALL_PRESENTATION_MS),
    );
    return finiteNonNegative(fromBytesPerSec) * (1 - fraction);
  }

  #recalculateRates(): void {
    const age = this.#evidenceMs - this.#regimeStartMs;
    const windowMs = presentationWindowMs(age);
    this.#presentedBytesPerSec = intervalRate(
      this.#observations,
      this.#evidenceMs - windowMs,
      this.#evidenceMs,
    );
    this.#fastBytesPerSec = intervalRate(
      this.#observations,
      Math.max(this.#regimeStartMs, this.#evidenceMs - REGIME_FAST_WINDOW_MS),
      this.#evidenceMs,
    );
  }

  #updateCandidate(observationStart: number): boolean {
    const age = this.#evidenceMs - this.#regimeStartMs;
    if (!this.#candidate) {
      if (age < REGIME_BASELINE_READY_MS || this.#presentedBytesPerSec <= 0)
        return false;
      const ratio = this.#fastBytesPerSec / this.#presentedBytesPerSec;
      if (ratio < REGIME_DOWNSHIFT_ENTER_RATIO) {
        this.#candidate = {
          kind: "down",
          start: observationStart,
          reference: this.#presentedBytesPerSec,
        };
      } else if (ratio > REGIME_UPSHIFT_ENTER_RATIO) {
        this.#candidate = {
          kind: "up",
          start: observationStart,
          reference: this.#presentedBytesPerSec,
        };
      }
      return false;
    }

    const candidate = this.#candidate;
    const ratio =
      candidate.reference > 0 ? this.#fastBytesPerSec / candidate.reference : 1;
    const cancelled =
      candidate.kind === "down"
        ? ratio > REGIME_DOWNSHIFT_CANCEL_RATIO
        : ratio < REGIME_UPSHIFT_CANCEL_RATIO;
    if (cancelled) {
      this.#candidate = null;
      return false;
    }
    const confirmationMs =
      candidate.kind === "down"
        ? REGIME_DOWNSHIFT_CONFIRM_MS
        : REGIME_UPSHIFT_CONFIRM_MS;
    if (this.#evidenceMs - candidate.start < confirmationMs) return false;

    this.#regimeStartMs = candidate.start;
    this.#candidate = null;
    this.#regimeId++;
    return true;
  }

  #prune(): void {
    const age = this.#evidenceMs - this.#regimeStartMs;
    const presentationStart = this.#evidenceMs - presentationWindowMs(age);
    const fastStart = Math.max(
      this.#regimeStartMs,
      this.#evidenceMs - REGIME_FAST_WINDOW_MS,
    );
    const keepFrom = Math.min(
      presentationStart,
      fastStart,
      this.#candidate?.start ?? this.#evidenceMs,
    );
    while (
      this.#observations.length > 1 &&
      this.#observations[0].end <= keepFrom
    )
      this.#observations.shift();
  }
}
