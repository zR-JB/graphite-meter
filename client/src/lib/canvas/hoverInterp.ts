// Hover lookup assumes the runner supplies samples sorted by ascending `t`.
/** Linear lookup that refuses to cross an intentional series break. */
export function interpolateConnectedAt<T extends { t: number }>(
  samples: T[],
  t: number,
  pick: (sample: T) => number,
  connected: (left: T, right: T) => boolean,
): number | null {
  if (!samples.length) return null;
  const insertion = lowerBoundAt(samples, t);
  if (insertion < samples.length && samples[insertion].t === t)
    return pick(samples[insertion]);
  const left = samples[insertion - 1];
  const right = samples[insertion];
  if (!left || !right || !connected(left, right)) return null;
  const weight = (t - left.t) / (right.t - left.t || 1);
  return pick(left) * (1 - weight) + pick(right) * weight;
}
/** Index of the first sample at or after `t` (binary search). */
export function lowerBoundAt<T extends { t: number }>(
  samples: T[],
  t: number,
): number {
  let lo = 0;
  let hi = samples.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (samples[mid].t < t) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}
/** A hover needs a rate or latency bucket; loss-only buckets retain ping evidence without an RTT. */
export function hasHoverMeasurements(info: {
  bytesPerSec: number | null;
  downBytesPerSec: number | null;
  upBytesPerSec: number | null;
  rtt: number | null;
  pingCount: number;
}): boolean {
  return (
    info.bytesPerSec != null ||
    info.downBytesPerSec != null ||
    info.upBytesPerSec != null ||
    info.rtt != null ||
    info.pingCount > 0
  );
}
