/** Fixed overlays use the visible viewport, including pinch zoom and panning. */
export function floatingViewport() {
  const viewport = window.visualViewport;
  const left = viewport?.offsetLeft ?? 0;
  const top = viewport?.offsetTop ?? 0;
  const width = viewport?.width ?? innerWidth;
  const height = viewport?.height ?? innerHeight;
  return {
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
  };
}

export function clampFloatingPosition(
  element: HTMLElement,
  left: number,
  top: number,
  viewport: ReturnType<typeof floatingViewport>,
) {
  element.style.left = `${Math.max(viewport.left + 8, Math.min(left, viewport.right - element.offsetWidth - 8))}px`;
  element.style.top = `${Math.max(viewport.top + 8, Math.min(top, viewport.bottom - element.offsetHeight - 8))}px`;
}

export function positionPopover(
  panel: HTMLElement,
  anchor: DOMRect,
  size: { width: number; minHeight: number; maxHeight: number },
) {
  const viewport = floatingViewport();
  if (
    anchor.bottom < viewport.top ||
    anchor.top > viewport.bottom ||
    anchor.right < viewport.left ||
    anchor.left > viewport.right
  ) {
    panel.hidePopover();
    return;
  }
  const below = viewport.bottom - anchor.bottom - 14;
  const above = anchor.top - viewport.top - 14;
  panel.style.width = `${Math.max(0, Math.min(size.width, viewport.width - 16))}px`;
  panel.style.maxHeight = `${Math.max(0, Math.min(viewport.height - 16, Math.max(size.minHeight, Math.min(size.maxHeight, Math.max(below, above)))))}px`;
  clampFloatingPosition(
    panel,
    anchor.left,
    below >= panel.offsetHeight || below >= above
      ? anchor.bottom + 6
      : anchor.top - panel.offsetHeight - 6,
    viewport,
  );
}
