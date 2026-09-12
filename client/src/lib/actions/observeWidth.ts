/** Width-driven DOM changes must happen outside ResizeObserver delivery. */
export function observeWidth(
  node: HTMLElement,
  onWidth: (width: number) => void,
) {
  let width = node.clientWidth;
  let frame = 0;
  onWidth(width);
  const observer = new ResizeObserver(() => {
    if (frame || node.clientWidth === width) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      const next = node.clientWidth;
      if (next !== width) {
        width = next;
        onWidth(width);
      }
    });
  });
  observer.observe(node);
  return {
    destroy() {
      observer.disconnect();
      if (frame) cancelAnimationFrame(frame);
    },
  };
}
