/* The Graphite Meter: closed-loop transfer sizer (shared). */

export interface SizerCfg {
  /** Wall-time each transfer aims to span. */
  targetMs: number;
  /** Lower / upper size clamp (bytes). */
  minBytes: number;
  maxBytes: number;
  /** Observed-rate EWMA weight (0..1); higher tracks faster, noisier. */
  alpha: number;
  /** Per-step growth / shrink clamp, the hysteresis (e.g. 2 and 0.5). */
  stepUp: number;
  stepDown: number;
}

/* Given the bytes/elapsed just observed and the running rate EWMA, return the next size and the updated EWMA. */
export function nextTransferBytes(
  prevBytes: number,
  elapsedMs: number,
  prevEwma: number,
  cfg: SizerCfg,
): { bytes: number; ewma: number } {
  if (elapsedMs <= 0) return { bytes: prevBytes, ewma: prevEwma };
  const observed = (prevBytes / elapsedMs) * 1000; // bytes/sec
  const ewma =
    prevEwma === 0
      ? observed
      : cfg.alpha * observed + (1 - cfg.alpha) * prevEwma;
  const want = (ewma * cfg.targetMs) / 1000;
  const stepped = Math.min(
    prevBytes * cfg.stepUp,
    Math.max(prevBytes * cfg.stepDown, want),
  );
  return {
    bytes: Math.floor(Math.min(cfg.maxBytes, Math.max(cfg.minBytes, stepped))),
    ewma,
  };
}
