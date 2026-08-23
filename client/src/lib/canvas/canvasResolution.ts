const BASE_PIXEL_RATIO_MAX = 2;
const PINCH_PIXEL_RATIO_MAX = 3;
const PINCH_PIXEL_RATIO_STEP = 0.5;

export function canvasPixelRatio(
  devicePixelRatio = window.devicePixelRatio || 1,
  viewportScale = window.visualViewport?.scale ?? 1,
): number {
  const base = Math.min(Math.max(devicePixelRatio, 1), BASE_PIXEL_RATIO_MAX);
  if (!Number.isFinite(viewportScale) || viewportScale <= 1) return base;
  return Math.min(
    Math.ceil((base * viewportScale) / PINCH_PIXEL_RATIO_STEP) *
      PINCH_PIXEL_RATIO_STEP,
    PINCH_PIXEL_RATIO_MAX,
  );
}

export function watchCanvasPixelRatio(onChange: () => void): () => void {
  const viewport = window.visualViewport;
  if (!viewport) return () => {};
  let ratio = canvasPixelRatio();
  const onResize = () => {
    const next = canvasPixelRatio();
    if (next === ratio) return;
    ratio = next;
    onChange();
  };
  viewport.addEventListener("resize", onResize);
  return () => viewport.removeEventListener("resize", onResize);
}
