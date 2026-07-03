/* ============================================================
 * The Graphite Meter — Sample statistics helpers
 * Pure, engine-agnostic descriptors over raw sample arrays.
 * Extracted from dummy.ts so the dummy and a real runner reduce
 * samples identically (shared by the evaluation core).
 * ============================================================ */

/** Median of an unsorted array; 0 for empty. */
export function median(xs: number[]): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

/** Nearest-rank percentile (p in 0–100); 0 for empty. */
export function percentile(xs: number[], p: number): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const idx = Math.min(
    s.length - 1,
    Math.max(0, Math.ceil((p / 100) * s.length) - 1),
  );
  return s[idx];
}

/** Mean of absolute consecutive differences — the jitter proxy; 0 for <2 samples. */
export function meanAbsDeviation(xs: number[]): number {
  if (xs.length < 2) return 0;
  let acc = 0;
  for (let i = 1; i < xs.length; i++) acc += Math.abs(xs[i] - xs[i - 1]);
  return acc / (xs.length - 1);
}
