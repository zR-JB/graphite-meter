import type { Attachment } from "svelte/attachments";
import { nextFrame } from "../presentation/motion.svelte";

/** Width-driven state changes happen outside ResizeObserver delivery. */
export function observeWidth(
  onWidth: (width: number) => void,
): Attachment<HTMLElement> {
  return (node) => {
    let width = node.clientWidth;
    let stop: (() => void) | null = null;
    onWidth(width);
    const observer = new ResizeObserver(() => {
      if (stop || node.clientWidth === width) return;
      stop = nextFrame(() => {
        stop = null;
        if (node.clientWidth === width) return;
        width = node.clientWidth;
        onWidth(width);
      });
    });
    observer.observe(node);
    return () => {
      observer.disconnect();
      stop?.();
    };
  };
}
