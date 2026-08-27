/* Canonical ping outcome crossing the worker boundary. */
export interface PingSample {
  rtt: number;
  lost: boolean;
  /** `performance.timeOrigin + performance.now()` in the worker realm. */
  observedAtEpochMs: number;
}

export function pingSample(
  rtt: number,
  lost: boolean,
  observedAtMs = performance.now(),
  timeOriginMs = performance.timeOrigin,
): PingSample {
  return {
    rtt,
    lost,
    observedAtEpochMs: timeOriginMs + observedAtMs,
  };
}

/* Performance time origins are epoch-based and therefore remain comparable even when the worker and window have. */
export function pingSampleContextTime(
  sample: PingSample,
  timeOriginMs = performance.timeOrigin,
): number {
  return sample.observedAtEpochMs - timeOriginMs;
}
