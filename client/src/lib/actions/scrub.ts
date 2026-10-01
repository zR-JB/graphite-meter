// A plot answers a mouse at once; a finger reads it on a tap or a sideways drag, and a vertical swipe scrolls past.
const SCRUB_PX = 8;

interface Scrub {
  /** Show the reading under this event's pointer. */
  read: (event: PointerEvent) => void;
  /** The reading ends: a drag lifted, or the page took the finger to scroll. */
  clear: (event: PointerEvent) => void;
}

/** Pointer handlers for a plot whose element sets `touch-action: pan-y pinch-zoom`. */
export function scrub({ read, clear }: Scrub) {
  let touch: { id: number; x: number; y: number; dragging: boolean } | null =
    null;
  return {
    down(event: PointerEvent) {
      if (event.pointerType !== "touch") return read(event);
      touch = {
        id: event.pointerId,
        x: event.clientX,
        y: event.clientY,
        dragging: false,
      };
    },
    move(event: PointerEvent) {
      if (event.pointerType !== "touch") return read(event);
      if (touch?.id !== event.pointerId) return;
      const dx = Math.abs(event.clientX - touch.x);
      if (
        !touch.dragging &&
        (dx < SCRUB_PX || dx < Math.abs(event.clientY - touch.y))
      )
        return;
      touch.dragging = true;
      read(event);
    },
    // A tap keeps its reading until focus moves on; a drag reads only while the finger is down.
    up(event: PointerEvent) {
      if (touch?.id !== event.pointerId) return;
      if (touch.dragging) clear(event);
      else read(event);
      touch = null;
    },
    cancel(event: PointerEvent) {
      if (touch?.id !== event.pointerId) return;
      touch = null;
      clear(event);
    },
  };
}
