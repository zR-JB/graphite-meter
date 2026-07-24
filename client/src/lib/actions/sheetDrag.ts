export interface SheetDragOptions {
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

export type SheetGestureIntent = "pending" | "drag" | "scroll";

// Stays "pending" inside a 10px slop radius so a tap never nudges the sheet.
// Anything but a downward, mostly-vertical pull from an unscrolled body belongs
// to the content underneath, and the sheet must not steal it.
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

// Either a pull past roughly a quarter of the sheet, or a shorter flick that was
// still moving when the finger left: the time bound keeps a fast drag that ended
// in a pause from counting as a flick.
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

// Reference counted because several sheets can be mounted at once, and the
// first one to close must not restore the page while another still holds it.
let pageLockCount = 0;
let pageBeforeLock:
  | {
      scrollY: number;
      bodyCss: string;
      rootOverscroll: string;
    }
  | undefined;

// `overflow: hidden` alone does not stop scrolling behind a sheet on iOS, so the
// body is pinned with `position: fixed` and offset to fake the scroll position.
function lockPage() {
  pageLockCount++;
  if (pageLockCount !== 1) return;
  const body = document.body;
  pageBeforeLock = {
    scrollY: window.scrollY,
    bodyCss: body.style.cssText,
    rootOverscroll: document.documentElement.style.overscrollBehavior,
  };
  document.documentElement.style.overscrollBehavior = "none";
  Object.assign(body.style, {
    position: "fixed",
    top: `-${pageBeforeLock.scrollY}px`,
    left: "0",
    right: "0",
    width: "100%",
    overflow: "hidden",
    overscrollBehavior: "none",
  });
}

function unlockPage() {
  if (!pageLockCount || --pageLockCount) return;
  const saved = pageBeforeLock;
  pageBeforeLock = undefined;
  if (!saved) return;
  document.body.style.cssText = saved.bodyCss;
  document.documentElement.style.overscrollBehavior = saved.rootOverscroll;
  window.scrollTo(0, saved.scrollY);
}

export function sheetDrag(node: HTMLElement, options: SheetDragOptions) {
  let opts = options;
  let pageLocked = false;
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

  function setPageLocked(locked: boolean) {
    if (locked === pageLocked) return;
    pageLocked = locked;
    if (locked) lockPage();
    else unlockPage();
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

  // Only the narrow layout renders the panel as a bottom sheet; wider viewports
  // dock it, where a downward drag would mean nothing.
  function onStart(event: TouchEvent) {
    if (
      !opts.enabled ||
      event.touches.length !== 1 ||
      !window.matchMedia("(max-width: 759px)").matches
    )
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
    // Smoothed, because a single frame's delta is noisy enough to read a steady
    // drag as a flick.
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
      // Inline styles come off only once the --dur-slide transition has run;
      // dropping them mid-flight would snap the sheet instead of gliding it.
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

  node.addEventListener("touchstart", onStart, { passive: true });
  // Non-passive: a committed drag has to preventDefault to suppress the browser's
  // own scroll and pull-to-refresh.
  node.addEventListener("touchmove", onMove, { passive: false });
  node.addEventListener("touchend", onEnd, { passive: true });
  node.addEventListener("touchcancel", onEnd, { passive: true });
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
      node.removeEventListener("touchstart", onStart);
      node.removeEventListener("touchmove", onMove);
      node.removeEventListener("touchend", onEnd);
      node.removeEventListener("touchcancel", onEnd);
      setPageLocked(false);
      reset();
    },
  };
}
