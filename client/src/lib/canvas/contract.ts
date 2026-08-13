// Canvas engines pull state on invalidation; the shared presentation scheduler
// owns animation frames, visibility, and parking.

export interface CanvasEngine {
  attach(canvas: HTMLCanvasElement): void;
  wake(): void;
  /** Apply the sizing owner's CSS-pixel canvas rectangle when it owns layout. */
  resize?(cssWidth: number, cssHeight: number): void;
  /** Re-resolve theme CSS vars without measuring or resizing the canvas. */
  invalidateTheme(): void;
  destroy(): void;
}
