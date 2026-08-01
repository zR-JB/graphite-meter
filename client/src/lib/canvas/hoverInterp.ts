// Time-indexed sample lookup for the chart's hover readout. Both helpers
// assume the array is sorted ascending by `t`, which the runner guarantees.

/** Linearly interpolated value at time `t`, or null when `t` falls outside the
 *  sample range: callers render nothing rather than extrapolating. */
export function interpolateAt<T extends { t: number }>(
  samples: T[],
  t: number,
  pick: (s: T) => number,
): number | null {
  if (!samples.length) return null;
  const insertion = lowerBoundAt(samples, t);
  const left = insertion - 1;
  const right = insertion;
  if (right < samples.length && samples[right].t === t)
    return pick(samples[right]);
  if (left < 0 || right >= samples.length) return null;
  const a = samples[left];
  const b = samples[right];
  // `|| 1` keeps the weight finite if two samples share a timestamp.
  const weight = (t - a.t) / (b.t - a.t || 1);
  return pick(a) * (1 - weight) + pick(b) * weight;
}

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
