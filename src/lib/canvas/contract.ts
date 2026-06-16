/* ============================================================
 * The Graphite Meter — Canvas Engine Lifecycle Contract (§11)
 * Each canvas engine follows this framework-agnostic lifecycle
 * so Svelte components stay thin wrappers (mount/destroy only,
 * no per-frame Svelte reactivity). Engines PULL from the store's
 * ring buffers via their own rAF loop — never pushed per sample.
 * ============================================================ */

export interface CanvasEngine {
  attach(canvas: HTMLCanvasElement): void;
  /** Starts the engine's internal rAF loop. */
  start(): void;
  stop(): void;
  /** Re-resolve theme CSS vars + DPR on resize/theme change. */
  invalidateTheme(): void;
  destroy(): void;
}
