/* ============================================================
 * The Graphite Meter — closed-loop transfer sizer (shared)
 * ============================================================
 *
 * One pure function, used by the upload worker (POST size) and the experimental
 * chunked-download worker (request size). It sizes the NEXT transfer to a target
 * wall-duration from the LAST one's observed rate, so a single tool spans dial-up
 * to multi-Gbit: big transfers on a fast link, small responsive ones on a slow or
 * dropping link. The rate is EWMA-smoothed (per-lane, so lanes can't synchronise
 * into oscillation) and the size is step-clamped (the hysteresis) and bounded.
 * There are NO preemptive kills — the resize only governs the next transfer.
 * ============================================================ */

export interface SizerCfg {
  /** Wall-time each transfer aims to span. */
  targetMs: number;
  /** Lower / upper size clamp (bytes). */
  minBytes: number;
  maxBytes: number;
  /** Observed-rate EWMA weight (0..1); higher tracks faster, noisier. */
  alpha: number;
  /** Per-step growth / shrink clamp — the hysteresis (e.g. 2 and 0.5). */
  stepUp: number;
  stepDown: number;
}

/** Given the bytes/elapsed just observed and the running rate EWMA, return the
 *  next size and the updated EWMA. `prevEwma === 0` seeds from the first sample. */
export function nextTransferBytes(
  prevBytes: number,
  elapsedMs: number,
  prevEwma: number,
  c: SizerCfg,
): { bytes: number; ewma: number } {
  if (elapsedMs <= 0) return { bytes: prevBytes, ewma: prevEwma };
  const observed = (prevBytes / elapsedMs) * 1000; // bytes/sec
  const ewma =
    prevEwma === 0 ? observed : c.alpha * observed + (1 - c.alpha) * prevEwma;
  const want = (ewma * c.targetMs) / 1000;
  const stepped = Math.min(
    prevBytes * c.stepUp,
    Math.max(prevBytes * c.stepDown, want),
  );
  return {
    bytes: Math.floor(Math.min(c.maxBytes, Math.max(c.minBytes, stepped))),
    ewma,
  };
}
