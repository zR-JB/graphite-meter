const BASE_PIXEL_RATIO_MAX = 2;
const PINCH_PIXEL_RATIO_MAX = 4;
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
  let ratio = canvasPixelRatio();
  let density = window.matchMedia(
    `(resolution: ${window.devicePixelRatio}dppx)`,
  );
  const onResize = () => {
    const next = canvasPixelRatio();
    if (next === ratio) return;
    ratio = next;
    onChange();
  };
  const onDensity = () => {
    density.removeEventListener("change", onDensity);
    density = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    density.addEventListener("change", onDensity);
    onResize();
  };
  const onVisibility = () => {
    if (!document.hidden) onDensity();
  };
  density.addEventListener("change", onDensity);
  viewport?.addEventListener("resize", onResize);
  window.addEventListener("resize", onResize);
  document.addEventListener("visibilitychange", onVisibility);
  return () => {
    density.removeEventListener("change", onDensity);
    viewport?.removeEventListener("resize", onResize);
    window.removeEventListener("resize", onResize);
    document.removeEventListener("visibilitychange", onVisibility);
  };
}
