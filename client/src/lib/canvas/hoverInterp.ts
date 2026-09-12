import { monotoneCurve } from "./smoothPath";

// Hover lookup assumes the runner supplies samples sorted by ascending `t`.
/** Follow the plotted curve without crossing an intentional series break. */
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
  const before = samples[insertion - 2];
  const after = samples[insertion + 1];
  const hasBefore = before && connected(before, left);
  const neighbors = [
    ...(hasBefore ? [before] : []),
    left,
    right,
    ...(after && connected(right, after) ? [after] : []),
  ];
  const curve = monotoneCurve(
    neighbors.map((sample) => ({ x: sample.t, y: pick(sample) })),
  )[hasBefore ? 1 : 0];
  const u = (t - left.t) / (right.t - left.t);
  const v = 1 - u;
  return (
    v ** 3 * pick(left) +
    3 * v ** 2 * u * curve.control1.y +
    3 * v * u ** 2 * curve.control2.y +
    u ** 3 * pick(right)
  );
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
