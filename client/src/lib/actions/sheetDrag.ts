export interface SheetDragOptions {
  enabled: boolean;
  backdrop?: HTMLElement;
  onDismiss: () => void;
}

interface SnapInput {
  distance: number;
  height: number;
  velocity: number;
  releasedAfterMs: number;
}

export function shouldDismissSheet({
  distance,
  height,
  velocity,
  releasedAfterMs,
}: SnapInput): boolean {
  const farEnough = distance >= Math.min(160, height * 0.28);
  const recentFlick =
    distance >= 48 && velocity >= 0.75 && releasedAfterMs <= 80;
  return farEnough || recentFlick;
}

export function sheetDrag(node: HTMLElement, options: SheetDragOptions) {
  let opts = options;
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
      }
    | undefined;

  const interactive = (target: EventTarget | null) =>
    target instanceof Element &&
    !!target.closest(
      "button, a, label, input, select, textarea, summary, [contenteditable='true']",
    );

  const reducedMotion = () =>
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

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
    if (
      !opts.enabled ||
      event.touches.length !== 1 ||
      !window.matchMedia("(max-width: 759px)").matches ||
      interactive(event.target)
    )
      return;
    const touch = event.touches[0];
    gesture = {
      id: touch.identifier,
      startX: touch.clientX,
      startY: touch.clientY,
      lastY: touch.clientY,
      lastAt: event.timeStamp,
      velocity: 0,
      dragging: false,
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
      if (Math.max(Math.abs(deltaX), Math.abs(deltaY)) < 10) return;
      const body =
        event.target instanceof Element
          ? event.target.closest(".panel-body")
          : null;
      if (
        deltaY <= 0 ||
        Math.abs(deltaX) > Math.abs(deltaY) ||
        (body instanceof HTMLElement && body.scrollTop > 0)
      ) {
        gesture = undefined;
        return;
      }
      gesture.dragging = true;
    }

    event.preventDefault();
    const elapsed = Math.max(1, event.timeStamp - gesture.lastAt);
    const instantVelocity = (touch.clientY - gesture.lastY) / elapsed;
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
      resetTimer = window.setTimeout(reset, 200);
      return;
    }

    if (reducedMotion()) {
      opts.onDismiss();
      reset();
      return;
    }
    animate(node.offsetHeight, true);
    resetTimer = window.setTimeout(() => {
      opts.onDismiss();
      reset();
    }, 180);
  }

  node.addEventListener("touchstart", onStart, { passive: true });
  node.addEventListener("touchmove", onMove, { passive: false });
  node.addEventListener("touchend", onEnd, { passive: true });
  node.addEventListener("touchcancel", onEnd, { passive: true });

  return {
    update(next: SheetDragOptions) {
      opts = next;
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
      reset();
    },
  };
}
