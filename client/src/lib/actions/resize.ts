import type { Attachment } from "svelte/attachments";

interface Resize {
  /** The side the resized element sits on; its handle is on the opposite edge. */
  side: "left" | "right";
  /** The width the handle reports. */
  width: () => number;
  min: number;
  max: () => number;
  set: (px: number) => void;
  reset: () => void;
}

/** A slider on an element's edge: a drag, the arrows (16 px, 48 with Shift), Home and End set its width within limits; Enter, Space or a double-click resets it. */
export function resize({
  side,
  width,
  min,
  max,
  set,
  reset,
}: Resize): Attachment<HTMLElement> {
  const clamped = (px: number) => set(Math.max(min, Math.min(max(), px)));
  return (handle) => {
    let finish: (() => void) | undefined;
    const start = (event: PointerEvent) => {
      if (!event.isPrimary || event.button !== 0) return;
      finish?.();
      event.preventDefault();
      const startX = event.clientX;
      const startWidth = width();
      const { cursor, userSelect } = document.body.style;
      handle.setPointerCapture(event.pointerId);
      document.body.style.userSelect = "none";
      document.body.style.cursor = "col-resize";
      const move = (next: PointerEvent) => {
        if (next.pointerId === event.pointerId) {
          const delta = next.clientX - startX;
          clamped(startWidth + (side === "left" ? delta : -delta));
        }
      };
      const end = (next: PointerEvent) => {
        if (next.pointerId === event.pointerId) finish?.();
      };
      finish = () => {
        finish = undefined;
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", end);
        handle.removeEventListener("pointercancel", end);
        handle.removeEventListener("lostpointercapture", end);
        if (handle.hasPointerCapture(event.pointerId))
          handle.releasePointerCapture(event.pointerId);
        document.body.style.cursor = cursor;
        document.body.style.userSelect = userSelect;
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", end);
      handle.addEventListener("pointercancel", end);
      handle.addEventListener("lostpointercapture", end);
    };
    const key = (event: KeyboardEvent) => {
      const now = width();
      const step = event.shiftKey ? 48 : 16;
      const next: Record<string, number> = {
        ArrowRight: now + step,
        ArrowUp: now + step,
        ArrowLeft: now - step,
        ArrowDown: now - step,
        Home: min,
        End: max(),
      };
      if (event.key === "Enter" || event.key === " ") reset();
      else if (event.key in next) clamped(next[event.key]);
      else return;
      event.preventDefault();
    };
    handle.addEventListener("pointerdown", start);
    handle.addEventListener("keydown", key);
    handle.addEventListener("dblclick", reset);
    return () => {
      finish?.();
      handle.removeEventListener("pointerdown", start);
      handle.removeEventListener("keydown", key);
      handle.removeEventListener("dblclick", reset);
    };
  };
}
