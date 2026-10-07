import {
  animate,
  spring,
  still,
  type Axis,
} from "../presentation/motion.svelte";

// A finger inside this radius is a tap, not a drag; the sheet then follows from where it left the radius.
const SLOP_PX = 10;
// The finger's speed is read over its last moves, so one noisy step is not a flick and a held finger is still.
const VELOCITY_WINDOW_MS = 100;
const HELD_MS = 60;
// Where the sheet would come to rest if the hand let go of it: its offset plus this long at its speed. It leaves
// once that passes a share of its size, so a short quick flick and a slow long drag both send it away, and a flick
// back toward rest keeps it.
const PROJECT_MS = 400;
const LEAVE_SHARE = 0.4;
const LEAVE_MAX_PX = 240;
// Pulled the wrong way, the sheet gives half as far, then ever less, never more than this.
const GIVE_PX = 56;
// Released, the sheet springs back to rest within this, or out until it crosses its edge.
const SETTLE_MS = 600;

type Edge = "bottom" | "right" | "left";
const rubber = (offset: number) =>
  offset >= 0 ? offset : -GIVE_PX * (1 - 1 / (1 - offset / GIVE_PX / 2));

/** Drags a sheet out by its edge: a portrait bottom sheet down, a side flyout toward its side. It follows the
 *  finger, and on release carries the finger's speed into the console's spring, to rest or out; a touch while it
 *  springs catches it where it is. `--sheet-drag` on its parent fades the sibling scrim. */
export function sheetDrag(onDismiss: () => void) {
  return (node: HTMLElement) => {
    const host = node.parentElement ?? node;
    const edge = (): Edge =>
      matchMedia("(max-width: 759px) and (orientation: portrait)").matches
        ? "bottom"
        : node.classList.contains("left")
          ? "left"
          : "right";
    // How far out the sheet goes, as its closed place in CSS: its height, or its width and the gap it slides past.
    const extent = (at: Edge) =>
      at === "bottom"
        ? node.offsetHeight
        : node.offsetWidth +
          (parseFloat(getComputedStyle(node).getPropertyValue("--space-4")) ||
            0);
    // The shown offset outward from rest.
    let offset = 0;
    let stop: (() => void) | undefined;
    let gesture:
      | {
          id: number;
          at: Edge;
          size: number;
          startX: number;
          startY: number;
          // The finger's position along the dismissing direction where the drag took hold.
          origin: number | null;
          from: number;
          samples: { t: number; p: number }[];
          scroller: HTMLElement | null;
        }
      | undefined;

    function place(at: Edge, shown: number, size: number) {
      offset = shown;
      node.style.transition = "none";
      node.style.transform =
        at === "bottom"
          ? `translateY(${shown}px)`
          : `translateX(${at === "left" ? -shown : shown}px)`;
      host.style.setProperty(
        "--sheet-drag",
        String(Math.max(0, Math.min(1, shown / size))),
      );
    }
    function reset() {
      stop?.();
      stop = undefined;
      offset = 0;
      node.style.transition = "";
      node.style.transform = "";
      host.style.removeProperty("--sheet-drag");
    }
    // The finger's position along the direction that dismisses.
    const along = (at: Edge, touch: Touch) =>
      at === "bottom"
        ? touch.clientY
        : at === "right"
          ? touch.clientX
          : -touch.clientX;
    function onStart(event: TouchEvent) {
      if (event.touches.length !== 1 || !(event.target instanceof Element))
        return;
      // A control that takes a sideways or held finger keeps it.
      if (event.target.closest("input, select, textarea, [role='slider']"))
        return;
      const at = edge();
      const touch = event.touches[0];
      // A finger on a sheet that is still springing catches it where it is.
      const caught = !!stop;
      stop?.();
      stop = undefined;
      gesture = {
        id: touch.identifier,
        at,
        size: extent(at),
        startX: touch.clientX,
        startY: touch.clientY,
        origin: caught ? along(at, touch) : null,
        from: caught ? offset : 0,
        samples: [],
        scroller: event.target.closest<HTMLElement>(".panel-body"),
      };
    }
    function onMove(event: TouchEvent) {
      if (!gesture) return;
      const touch = [...event.touches].find(
        (candidate) => candidate.identifier === gesture!.id,
      );
      if (!touch) return;
      const { at } = gesture;
      if (gesture.origin === null) {
        const dx = touch.clientX - gesture.startX;
        const dy = touch.clientY - gesture.startY;
        if (Math.hypot(dx, dy) < SLOP_PX) return;
        const outward = at === "bottom" ? dy : at === "right" ? dx : -dx;
        const across = at === "bottom" ? dx : dy;
        // Out along the sheet's axis drags it; anything else scrolls or belongs to the content. Down drags a
        // bottom sheet only from its top, so the list under the finger scrolls first.
        const scrolled =
          at === "bottom" && (gesture.scroller?.scrollTop ?? 0) > 0;
        if (outward <= 0 || Math.abs(across) > Math.abs(outward) || scrolled) {
          gesture = undefined;
          return;
        }
        gesture.origin = along(at, touch);
      }
      event.preventDefault();
      const position = along(at, touch);
      gesture.samples.push({ t: event.timeStamp, p: position });
      while (
        gesture.samples.length > 2 &&
        event.timeStamp - gesture.samples[0].t > VELOCITY_WINDOW_MS
      )
        gesture.samples.shift();
      place(at, rubber(gesture.from + position - gesture.origin), gesture.size);
    }
    function onEnd(event: TouchEvent) {
      if (!gesture) return;
      const ended = [...event.changedTouches].some(
        (candidate) => candidate.identifier === gesture!.id,
      );
      if (!ended) return;
      const { at, size, samples, origin } = gesture;
      gesture = undefined;
      if (origin === null) return;
      const first = samples[0];
      const last = samples.at(-1);
      const velocity =
        first && last && last.t > first.t && event.timeStamp - last.t <= HELD_MS
          ? (last.p - first.p) / (last.t - first.t)
          : 0;
      const leave =
        event.type === "touchend" &&
        offset + velocity * PROJECT_MS >
          Math.min(size * LEAVE_SHARE, LEAVE_MAX_PX);
      release(at, size, velocity, leave);
    }
    // The sheet springs from where the finger left it at the finger's speed: out past its edge, or back to rest
    // with the console's slight bob.
    function release(at: Edge, size: number, velocity: number, leave: boolean) {
      const target = leave ? size : 0;
      const finish = () => {
        stop = undefined;
        if (leave) {
          place(at, size, size);
          onDismiss();
        }
        reset();
      };
      if (still()) return finish();
      const from: Axis = [offset - target, velocity];
      let began: number | undefined;
      stop = animate((now) => {
        began ??= now;
        const t = now - began;
        const [x] = spring(from, t);
        if ((leave && x >= 0) || t >= SETTLE_MS) {
          finish();
          return false;
        }
        place(at, target + x, size);
        return true;
      });
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
