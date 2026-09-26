import { prefersReducedMotion } from "svelte/motion";

// Stays "pending" inside a 10px slop radius so a tap never nudges the sheet.
function gestureIntent(deltaX: number, deltaY: number, scrollTop: number) {
  if (Math.max(Math.abs(deltaX), Math.abs(deltaY)) < 10) return "pending";
  if (deltaY <= 0 || Math.abs(deltaX) > Math.abs(deltaY) || scrollTop > 0)
    return "scroll";
  return "drag";
}

// A flick counts only while the finger still moves at release.
function shouldDismiss(
  distance: number,
  height: number,
  velocity: number,
  releasedAfterMs: number,
) {
  const farEnough = distance >= Math.min(160, height * 0.28);
  const recentFlick =
    distance >= 96 && velocity >= 0.85 && releasedAfterMs <= 80;
  return farEnough || recentFlick;
}

/** Drags a portrait bottom sheet down to dismiss it; `--sheet-drag` fades its backdrop. */
export function sheetDrag(onDismiss: () => void) {
  return (node: HTMLElement) => {
    let settle: ReturnType<typeof setTimeout> | undefined;
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
    const bottomSheet = () =>
      matchMedia("(max-width: 759px) and (orientation: portrait)").matches;
    const slideMs = () =>
      prefersReducedMotion.current
        ? 0
        : parseFloat(getComputedStyle(node).getPropertyValue("--dur-slide"));
    function reset() {
      clearTimeout(settle);
      node.style.transition = "";
      node.style.transform = "";
      node.style.removeProperty("--sheet-drag");
    }
    function animate(offset: number, ms: number) {
      node.style.transition = ms ? `transform ${ms}ms var(--ease-out)` : "none";
      node.style.transform = `translateY(${offset}px)`;
      node.style.setProperty(
        "--sheet-drag",
        String(Math.min(1, offset / node.offsetHeight)),
      );
    }
    function onStart(event: TouchEvent) {
      if (event.touches.length !== 1 || !bottomSheet()) return;
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
        const intent = gestureIntent(
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
      animate(Math.max(0, deltaY), 0);
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
      const dismiss =
        event.type === "touchend" &&
        shouldDismiss(
          Math.max(0, touch.clientY - gesture.startY),
          node.offsetHeight,
          gesture.velocity,
          event.timeStamp - gesture.lastAt,
        );
      gesture = undefined;
      const ms = slideMs();
      animate(dismiss ? node.offsetHeight : 0, ms);
      // Dismiss as the slide-out lands, so the sheet never flashes back on screen.
      settle = setTimeout(() => {
        if (dismiss) onDismiss();
        reset();
      }, ms);
    }
    const listeners = [
      ["touchstart", onStart],
      ["touchmove", onMove],
      ["touchend", onEnd],
      ["touchcancel", onEnd],
    ] as const;
    for (const [type, listener] of listeners)
      node.addEventListener(type, listener, { passive: type !== "touchmove" });
    return () => {
      for (const [type, listener] of listeners)
        node.removeEventListener(type, listener);
      reset();
    };
  };
}
