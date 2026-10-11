// A finger drags a floating sheet out by its edge: an upright phone's bottom sheet down, a side flyout towards its
// side. The sheet follows the finger and, released, carries the finger's speed into the console's spring, to rest or
// out; a touch while it springs catches it where it is. A mouse never drags a sheet.
import { animate } from "../../lib/frame";
import { reducedMotion, spring } from "../../lib/motion";

export type Edge = "bottom" | "left" | "right";

/** Inside this radius a finger taps; past it the sheet follows from where the finger left it, so it never jumps. */
const SLOP_PX = 10;
/** The finger's speed is read over its last moves; one held still this long before lifting has none. */
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
  /** How far out the sheet goes to be hidden. */
  size: number;
  startX: number;
  startY: number;
  /** The finger's place along the dismissing direction where the drag took hold; null within the slop. */
  origin: number | null;
  from: number;
  samples: { t: number; p: number }[];
  scroller: Element | null;
}

/** Attaches the drag to `sheet` until the returned detach runs. `--sheet-drag` on its parent, 0 to 1, fades the
    scrim with it. */
export function dragSheet(sheet: HTMLElement, edge: () => Edge, onDismiss: () => void): () => void {
  const host = sheet.parentElement ?? sheet;
  let offset = 0;
  let stop: (() => void) | null = null;
  let gesture: Gesture | null = null;

  // Its closed place in CSS: its height, or its width and the gap its closed transform clears.
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
    const touch = event.touches[0];
    if (event.touches.length !== 1 || !touch || !(event.target instanceof Element)) return;
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
      scroller: event.target.closest(".panel-body"),
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
      // Down drags a bottom sheet only from the top of its list, so the list under the finger scrolls first.
      const scrolled = at === "bottom" && (current.scroller?.scrollTop ?? 0) > 0;
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
    gesture = null;
    if (current.origin === null) return;
    const { edge: at, size, samples } = current;
    const first = samples[0];
    const last = samples.at(-1);
    const velocity =
      first && last && last.t > first.t && event.timeStamp - last.t <= HELD_MS
        ? (last.p - first.p) / (last.t - first.t)
        : 0;
    const leave =
      event.type === "touchend" && offset + velocity * PROJECT_MS > Math.min(size * LEAVE_SHARE, LEAVE_MAX_PX);
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
