// A finger drags a floating sheet out by its edge. Released, it carries the finger's speed into the spring, to rest
// or out. A mouse never drags a sheet.
import { animate } from "../../lib/frame";
import { reducedMotion, spring } from "../../lib/motion";

export type Edge = "bottom" | "left" | "right";

/** Inside this radius a finger taps. Past it the sheet follows from where the finger left it, so it never jumps. */
const SLOP_PX = 10;
/** The finger's speed is read over its last moves. One held still this long before lifting has none. */
const VELOCITY_WINDOW_MS = 100;
const HELD_MS = 60;
/** Released, the sheet leaves when its offset plus this long at the finger's speed passes a share of its size, so a
    short quick flick and a slow long drag both send it away and a flick back keeps it. */
const PROJECT_MS = 400;
const LEAVE_SHARE = 0.4;
const LEAVE_MAX_PX = 240;
/** Pulled the wrong way, it gives half as far at first, then ever less, never more than this. */
const GIVE_PX = 56;
/** The spring settles at rest within this, or ends as it crosses out. */
const SETTLE_MS = 600;

const rubber = (offset: number) => (offset >= 0 ? offset : -GIVE_PX * (1 - 1 / (1 - offset / GIVE_PX / 2)));

interface Gesture {
  id: number;
  edge: Edge;
  /** The sheet goes this far out to be hidden. */
  size: number;
  startX: number;
  startY: number;
  /** The finger's place along the dismissing direction where the drag took hold, or null within the slop. */
  origin: number | null;
  from: number;
  samples: { t: number; p: number }[];
  /** Records where the finger landed in the content. The sheet's head or handle, which always drag, leave null. */
  content: Element | null;
}

/** Attaches the drag to `sheet` until the returned detach runs. `--sheet-drag` on its parent, 0 to 1, fades the
    scrim with it. */
export function dragSheet(sheet: HTMLElement, edge: () => Edge, onDismiss: () => void): () => void {
  const host = sheet.parentElement ?? sheet;
  // Checks every box from the finger up to the sheet for a scroll from its top, since what scrolls depends on the
  // layout, the content's body or the sheet itself on a short screen.
  const scrolledAbove = (from: Element) => {
    for (let node: Element | null = from; node; node = node === sheet ? null : node.parentElement)
      if (node.scrollTop > 0) return true;
    return false;
  };
  let offset = 0;
  let stop: (() => void) | null = null;
  let gesture: Gesture | null = null;

  // The closed place in CSS is its height, or its width and the gap its closed transform clears.
  const extent = (at: Edge) =>
    at === "bottom"
      ? sheet.offsetHeight
      : sheet.offsetWidth + (Number.parseFloat(getComputedStyle(sheet).getPropertyValue("--space-4")) || 0);
  const along = (at: Edge, touch: Touch) =>
    at === "bottom" ? touch.clientY : at === "right" ? touch.clientX : -touch.clientX;
  const place = (at: Edge, shown: number, size: number) => {
    offset = shown;
    sheet.style.transition = "none";
    sheet.style.transform =
      at === "bottom" ? `translateY(${shown}px)` : `translateX(${at === "left" ? -shown : shown}px)`;
    host.style.setProperty("--sheet-drag", String(Math.max(0, Math.min(1, shown / size))));
  };
  const reset = () => {
    stop?.();
    stop = null;
    offset = 0;
    host.style.removeProperty("--sheet-drag");
    if (!sheet.style.transform) return;
    // The finger or the spring already brought it to the stylesheet's place, which takes over without a transition.
    sheet.style.removeProperty("transform");
    void getComputedStyle(sheet).transform;
    sheet.style.removeProperty("transition");
  };

  function start(event: TouchEvent) {
    // A second finger is a pinch, not a drag, so the sheet goes back to rest and the browser zooms.
    if (event.touches.length > 1) {
      if (gesture) settle(gesture, false, event.timeStamp);
      return;
    }
    const touch = event.touches[0];
    if (!touch || !(event.target instanceof Element)) return;
    // A control that takes a sideways or held finger keeps it.
    if (event.target.closest("input, select, textarea, [role='slider'], [role='separator']")) return;
    const at = edge();
    const caught = !!stop;
    stop?.();
    stop = null;
    gesture = {
      id: touch.identifier,
      edge: at,
      size: extent(at),
      startX: touch.clientX,
      startY: touch.clientY,
      origin: caught ? along(at, touch) : null,
      from: caught ? offset : 0,
      samples: [],
      content: event.target.closest(".sheet-head, .sheet-handle") ? null : event.target,
    };
  }

  function move(event: TouchEvent) {
    const current = gesture;
    const touch = current && [...event.touches].find((each) => each.identifier === current.id);
    if (!current || !touch) return;
    const at = current.edge;
    if (current.origin === null) {
      const dx = touch.clientX - current.startX;
      const dy = touch.clientY - current.startY;
      if (Math.hypot(dx, dy) < SLOP_PX) return;
      const outward = at === "bottom" ? dy : at === "right" ? dx : -dx;
      const across = at === "bottom" ? dx : dy;
      // Down drags a bottom sheet only from the top of its content, so the content under the finger scrolls first.
      const scrolled = at === "bottom" && !!current.content && scrolledAbove(current.content);
      if (outward <= 0 || Math.abs(across) > Math.abs(outward) || scrolled) {
        gesture = null;
        return;
      }
      current.origin = along(at, touch);
    }
    event.preventDefault();
    const position = along(at, touch);
    current.samples.push({ t: event.timeStamp, p: position });
    while (current.samples.length > 2 && event.timeStamp - current.samples[0]!.t > VELOCITY_WINDOW_MS)
      current.samples.shift();
    place(at, rubber(current.from + position - current.origin), current.size);
  }

  function end(event: TouchEvent) {
    const current = gesture;
    if (!current || ![...event.changedTouches].some((each) => each.identifier === current.id)) return;
    settle(current, event.type === "touchend", event.timeStamp);
  }

  /** Lets go of a drag. Lifted, the sheet leaves when offset and speed carry it far enough, else it returns to rest. */
  function settle(current: Gesture, lifted: boolean, time: number) {
    gesture = null;
    if (current.origin === null) return;
    const { edge: at, size, samples } = current;
    const first = samples[0];
    const last = samples.at(-1);
    const velocity =
      first && last && last.t > first.t && time - last.t <= HELD_MS ? (last.p - first.p) / (last.t - first.t) : 0;
    const leave = lifted && offset + velocity * PROJECT_MS > Math.min(size * LEAVE_SHARE, LEAVE_MAX_PX);
    const target = leave ? size : 0;
    // Left out of view, the route closes it and the stylesheet's closed place takes over where it already is.
    const finish = () => {
      stop = null;
      if (leave) {
        place(at, size, size);
        onDismiss();
      }
      reset();
    };
    if (reducedMotion.peek()) return finish();
    const released: [number, number] = [offset - target, velocity];
    let began: number | null = null;
    stop = animate((now) => {
      began ??= now;
      const [x] = spring(released, now - began);
      if ((leave && x >= 0) || now - began >= SETTLE_MS) {
        finish();
        return false;
      }
      place(at, target + x, size);
      return true;
    });
  }

  const listeners = [
    ["touchstart", start],
    ["touchmove", move],
    ["touchend", end],
    ["touchcancel", end],
  ] as const;
  for (const [type, listener] of listeners) sheet.addEventListener(type, listener, { passive: type !== "touchmove" });
  return () => {
    for (const [type, listener] of listeners) sheet.removeEventListener(type, listener);
    gesture = null;
    reset();
  };
}
