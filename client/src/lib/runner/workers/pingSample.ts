/** Canonical ping outcome crossing the worker boundary. RTT and loss describe
 * what happened; the absolute monotonic timestamp preserves when it happened
 * even when postMessage delivery is batched or delayed. */
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

/** Translate the cross-realm timestamp into this context's performance.now()
 * coordinate. Performance time origins are epoch-based and therefore remain
 * comparable even when the worker and window have different origins. */
export function pingSampleContextTime(
  sample: PingSample,
  timeOriginMs = performance.timeOrigin,
): number {
  return sample.observedAtEpochMs - timeOriginMs;
}
