/* ============================================================
 * Chart hover interpolation — the bracketing-sample math behind
 * ChartEngine's hoverInfo(). Pure so it's unit-testable without a canvas.
 * ============================================================ */

/** Linearly interpolate the value at time `t` between the two samples in
 *  `arr` (time-ordered) that bracket it. Returns null when `arr` is empty or
 *  `t` falls outside [arr[0].t, last.t] — the caller falls back to a nearest-
 *  sample lookup for that case. An exact hit on a sample's own `t` returns
 *  that sample's value with no blending. */
export function interpolateAt<T extends { t: number }>(
  arr: T[],
  t: number,
  pick: (s: T) => number,
): number | null {
  if (!arr.length) return null;
  if (t < arr[0].t || t > arr[arr.length - 1].t) return null;
  if (t === arr[0].t) return pick(arr[0]);
  const last = arr[arr.length - 1];
  if (t === last.t) return pick(last);
  for (let i = 1; i < arr.length; i++) {
    const b = arr[i];
    if (b.t >= t) {
      const a = arr[i - 1];
      const span = b.t - a.t || 1;
      const w = (t - a.t) / span;
      return pick(a) * (1 - w) + pick(b) * w;
    }
  }
  return null;
}

/** The sample in non-empty `arr` whose `t` is closest to `t`. */
export function closestSample<T extends { t: number }>(arr: T[], t: number): T {
  let closest = arr[0];
  let closestDist = Math.abs(arr[0].t - t);
  for (let i = 1; i < arr.length; i++) {
    const sample = arr[i];
    const dist = Math.abs(sample.t - t);
    if (dist < closestDist) {
      closestDist = dist;
      closest = sample;
    }
  }
  return closest;
}
