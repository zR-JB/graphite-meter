// Canvas engines pull state on invalidation; the shared presentation scheduler
// owns animation frames, visibility, and parking.

export interface CanvasEngine {
  attach(canvas: HTMLCanvasElement): void;
  wake(): void;
  /** Re-resolve theme CSS vars + DPR on resize/theme change. */
  invalidateTheme(): void;
  destroy(): void;
}
