/* ============================================================
 * The Graphite Meter — Canvas Engine Lifecycle Contract
 * Each canvas engine follows this framework-agnostic lifecycle
 * so Svelte components stay thin wrappers (mount/destroy only,
 * no per-frame Svelte reactivity). Engines PULL from the store's
 * ring buffers via their own rAF loop — never pushed per sample.
 * ============================================================ */

export interface CanvasEngine {
  attach(canvas: HTMLCanvasElement): void;
  /** Starts the engine's internal rAF loop. */
  start(): void;
  /** Re-arm the loop if it has self-parked. Idempotent while running.
   *  Engines park themselves once there is nothing left to animate (idle,
   *  settled) so the GPU goes quiet; callers wake() them on any change that
   *  warrants a redraw (state change, theme/resize, hover). */
  wake(): void;
  stop(): void;
  /** Re-resolve theme CSS vars + DPR on resize/theme change. */
  invalidateTheme(): void;
  destroy(): void;
}
