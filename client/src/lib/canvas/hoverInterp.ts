export function interpolateAt<T extends { t: number }>(
  arr: T[],
  t: number,
  pick: (s: T) => number,
): number | null {
  if (!arr.length) return null;
  const insertion = lowerBoundAt(arr, t);
  const left = insertion - 1;
  const right = insertion;
  if (right < arr.length && arr[right].t === t) return pick(arr[right]);
  if (left < 0 || right >= arr.length) return null;
  const a = arr[left];
  const b = arr[right];
  const weight = (t - a.t) / (b.t - a.t || 1);
  return pick(a) * (1 - weight) + pick(b) * weight;
}

export function lowerBoundAt<T extends { t: number }>(
  arr: T[],
  t: number,
): number {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (arr[mid].t < t) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}
