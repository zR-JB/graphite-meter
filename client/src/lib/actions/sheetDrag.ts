import { acquirePageScrollLock } from "./pageScrollLock";

interface SheetDragOptions {
  enabled: boolean;
  backdrop?: HTMLElement;
  onDismiss: () => void;
}
interface DismissInput {
  distance: number;
  height: number;
  velocity: number;
  releasedAfterMs: number;
}
type SheetGestureIntent = "pending" | "drag" | "scroll";
// Stays "pending" inside a 10px slop radius so a tap never nudges the sheet.
export function sheetGestureIntent(
  deltaX: number,
  deltaY: number,
  scrollTop: number,
): SheetGestureIntent {
  if (Math.max(Math.abs(deltaX), Math.abs(deltaY)) < 10) return "pending";
  if (deltaY <= 0 || Math.abs(deltaX) > Math.abs(deltaY) || scrollTop > 0)
    return "scroll";
  return "drag";
}
// A flick counts only while the finger still moves at release.
export function shouldDismissSheet({
  distance,
  height,
  velocity,
  releasedAfterMs,
}: DismissInput): boolean {
  const farEnough = distance >= Math.min(160, height * 0.28);
  const recentFlick =
    distance >= 96 && velocity >= 0.85 && releasedAfterMs <= 80;
  return farEnough || recentFlick;
}
export function sheetDrag(node: HTMLElement, options: SheetDragOptions) {
  let opts = options;
  let releasePageLock: (() => void) | undefined;
  let resetTimer: number | undefined;
  let gesture:
    | {
        id: number;
        startX: number;
        startY: number;
        lastY: number;
        lastAt: number;
        velocity: number;
        dragging: boolean;
        scroller?: HTMLElement;
      }
    | undefined;
  const reducedMotion = () =>
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  // Only portrait phones present this panel as a draggable bottom sheet.
  const isBottomSheetLayout = () =>
    window.matchMedia("(max-width: 759px) and (orientation: portrait)").matches;
  function setPageLocked(locked: boolean) {
    if (locked === Boolean(releasePageLock)) return;
    if (locked) releasePageLock = acquirePageScrollLock();
    else {
      releasePageLock?.();
      releasePageLock = undefined;
    }
  }
  function reset() {
    window.clearTimeout(resetTimer);
    node.style.transition = "";
    node.style.transform = "";
    if (opts.backdrop) {
      opts.backdrop.style.transition = "";
      opts.backdrop.style.opacity = "";
    }
  }
  function animate(offset: number, transition: boolean) {
    const motion =
      transition && !reducedMotion()
        ? "var(--dur-slide) var(--ease-out)"
        : "none";
    const progress = Math.min(1, offset / node.offsetHeight);
    node.style.transition = `transform ${motion}`;
    node.style.transform = `translateY(${offset}px)`;
    if (opts.backdrop) {
      opts.backdrop.style.transition = `opacity ${motion}`;
      opts.backdrop.style.opacity = String(1 - progress);
    }
  }
  function onStart(event: TouchEvent) {
    if (!opts.enabled || event.touches.length !== 1 || !isBottomSheetLayout())
      return;
    const touch = event.touches[0];
    const scroller =
      event.target instanceof Element
        ? event.target.closest<HTMLElement>(".panel-body")
        : null;
    gesture = {
      id: touch.identifier,
      startX: touch.clientX,
      startY: touch.clientY,
      lastY: touch.clientY,
      lastAt: event.timeStamp,
      velocity: 0,
      dragging: false,
      scroller: scroller ?? undefined,
    };
  }
  function onMove(event: TouchEvent) {
    if (!gesture) return;
    const touch = [...event.touches].find(
      (candidate) => candidate.identifier === gesture!.id,
    );
    if (!touch) return;
    const deltaX = touch.clientX - gesture.startX;
    const deltaY = touch.clientY - gesture.startY;
    if (!gesture.dragging) {
      const intent = sheetGestureIntent(
        deltaX,
        deltaY,
        gesture.scroller?.scrollTop ?? 0,
      );
      if (intent === "pending") return;
      if (intent === "scroll") {
        gesture = undefined;
        return;
      }
      gesture.dragging = true;
    }
    event.preventDefault();
    const elapsed = Math.max(1, event.timeStamp - gesture.lastAt);
    const instantVelocity = (touch.clientY - gesture.lastY) / elapsed;
    // Smoothed: a single frame's delta is noisy enough to read a steady drag as a flick.
    gesture.velocity = gesture.velocity * 0.65 + instantVelocity * 0.35;
    gesture.lastY = touch.clientY;
    gesture.lastAt = event.timeStamp;
    animate(Math.max(0, deltaY), false);
  }
  function onEnd(event: TouchEvent) {
    if (!gesture) return;
    const touch = [...event.changedTouches].find(
      (candidate) => candidate.identifier === gesture!.id,
    );
    if (!touch || !gesture.dragging) {
      gesture = undefined;
      return;
    }
    const dismiss = shouldDismissSheet({
      distance: Math.max(0, touch.clientY - gesture.startY),
      height: node.offsetHeight,
      velocity: gesture.velocity,
      releasedAfterMs: event.timeStamp - gesture.lastAt,
    });
    gesture = undefined;
    if (event.type === "touchcancel" || !dismiss) {
      if (reducedMotion()) {
        reset();
        return;
      }
      animate(0, true);
      // Inline styles come off once the --dur-slide transition finishes.
      resetTimer = window.setTimeout(reset, 200);
      return;
    }
    if (reducedMotion()) {
      opts.onDismiss();
      reset();
      return;
    }
    animate(node.offsetHeight, true);
    // Dismiss as the slide-out lands, so the sheet never flashes back on screen.
    resetTimer = window.setTimeout(() => {
      opts.onDismiss();
      reset();
    }, 180);
  }
  const listeners = [
    ["touchstart", onStart],
    ["touchmove", onMove],
    ["touchend", onEnd],
    ["touchcancel", onEnd],
  ] as const;
  for (const [type, listener] of listeners)
    node.addEventListener(type, listener as EventListener, {
      passive: type !== "touchmove",
    });
  setPageLocked(opts.enabled);
  return {
    update(next: SheetDragOptions) {
      opts = next;
      setPageLocked(opts.enabled);
      if (!opts.enabled) {
        gesture = undefined;
        reset();
      }
    },
    destroy() {
      for (const [type, listener] of listeners)
        node.removeEventListener(type, listener as EventListener);
      setPageLocked(false);
      reset();
    },
  };
}
