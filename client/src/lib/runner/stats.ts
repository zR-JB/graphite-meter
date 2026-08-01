/* ============================================================
 * The Graphite Meter: sample statistics helpers
 * Pure, engine-agnostic descriptors over raw sample arrays.
 * Shared by the evaluation core, so the dummy and a real runner
 * reduce samples identically.
 * ============================================================ */

/** Median of an unsorted array; 0 for empty. */
export function median(xs: number[]): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

/** Nearest-rank percentile (p in 0..100); 0 for empty. */
export function percentile(xs: number[], p: number): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const idx = Math.min(
    s.length - 1,
    Math.max(0, Math.ceil((p / 100) * s.length) - 1),
  );
  return s[idx];
}

/** Mean of absolute consecutive differences: the jitter proxy. 0 for <2 samples. */
export function meanAbsDeviation(xs: number[]): number {
  if (xs.length < 2) return 0;
  let acc = 0;
  for (let i = 1; i < xs.length; i++) acc += Math.abs(xs[i] - xs[i - 1]);
  return acc / (xs.length - 1);
}

export interface WeightedValue {
  value: number;
  weight: number;
}

/** Weighted center for bucket summaries whose observations represent different
 *  numbers of raw outcomes. Non-positive weights carry no evidence. */
export function weightedMean(values: WeightedValue[]): number | null {
  let weightedTotal = 0;
  let totalWeight = 0;
  for (const entry of values) {
    if (!(entry.weight > 0)) continue;
    weightedTotal += entry.value * entry.weight;
    totalWeight += entry.weight;
  }
  return totalWeight > 0 ? weightedTotal / totalWeight : null;
}

/** Mean absolute deviation using the same weights as the supplied center. */
export function weightedMeanAbsoluteDeviation(
  values: WeightedValue[],
  center: number,
): number | null {
  let weightedDeviation = 0;
  let totalWeight = 0;
  for (const entry of values) {
    if (!(entry.weight > 0)) continue;
    weightedDeviation += Math.abs(entry.value - center) * entry.weight;
    totalWeight += entry.weight;
  }
  return totalWeight > 0 ? weightedDeviation / totalWeight : null;
}
