import type { Attachment } from "svelte/attachments";

/** Width-driven state changes happen outside ResizeObserver delivery. */
export function observeWidth(
  onWidth: (width: number) => void,
): Attachment<HTMLElement> {
  return (node) => {
    let width = node.clientWidth;
    let frame = 0;
    onWidth(width);
    const observer = new ResizeObserver(() => {
      if (frame || node.clientWidth === width) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        if (node.clientWidth === width) return;
        width = node.clientWidth;
        onWidth(width);
      });
    });
    observer.observe(node);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  };
}
