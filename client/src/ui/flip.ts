// Animates layout moves. A change applies at once, then every `data-flip` element that stayed springs to its new
// place. `data-flip-resize` also springs its width. It and `data-flip-layout` move by offsets in layout, since
// Firefox draws a compositor translate a frame apart from a width that moves with it.
import { reducedMotion, spring, SPRING_MS } from "../lib/motion";

type Axis = [offset: number, velocity: number];
type Axes = { x: Axis; y: Axis; w: Axis };

/** Neighbours have moved most of the way by then, so an arriving element appears into room already made. */
const ROOM_MS = 140;
const STEPS = 30;

const moving = new WeakMap<Element, { axes: Axes; run: Animation }>();
let flipping = false;

/** Reads the speed of `el`'s move at its animation's own time, so moves started together read the same instant and
    a move still waiting for its first frame keeps its starting speed. */
const velocity = (el: Element): Axes => {
  const move = moving.get(el);
  const t = move ? Number(move.run.currentTime ?? SPRING_MS) : SPRING_MS;
  const v = (axis: keyof Axes): Axis => [0, t < SPRING_MS ? spring(move!.axes[axis], t)[1] : 0];
  return { x: v("x"), y: v("y"), w: v("w") };
};

/** Springs `el` from the given offsets to rest, drawn as keyframes over SPRING_MS. */
function settle(el: Element, axes: Axes, frame: (x: number, y: number, w: number) => Keyframe): void {
  const frames = Array.from({ length: STEPS + 1 }, (_, i) => {
    const t = (i / STEPS) * SPRING_MS;
    const at = (axis: Axis) => (i === STEPS ? 0 : spring(axis, t)[0]);
    return frame(at(axes.x), at(axes.y), at(axes.w));
  });
  moving.set(el, { axes, run: el.animate(frames, { duration: SPRING_MS }) });
}

/** Collects the displayed elements marked `data-flip`, by key. */
function boxes(only?: readonly string[]): Map<string, { el: HTMLElement; box: DOMRect }> {
  const found = new Map<string, { el: HTMLElement; box: DOMRect }>();
  for (const el of document.querySelectorAll<HTMLElement>("[data-flip]")) {
    const key = el.dataset.flip!;
    if (only && !only.includes(key)) continue;
    const box = el.getBoundingClientRect();
    if (box.width > 0 || box.height > 0) found.set(key, { el, box });
  }
  return found;
}

/** Applies `update` and moves what it moved. `only` names the keys that move, so a surface moves as one. A flip
    within a flip is part of the outer one. */
export function flip(update: () => void, only?: readonly string[]): void {
  if (flipping || reducedMotion.peek() || document.hidden) return update();
  const before = boxes(only);
  const speed = new Map([...before].map(([key, { el }]) => [key, velocity(el)]));
  // The new places are measured without the moves still under way.
  for (const { el } of before.values()) moving.get(el)?.run.cancel();
  flipping = true;
  try {
    update();
  } finally {
    flipping = false;
  }
  const after = boxes(only);
  const arriving: HTMLElement[] = [];
  let moved = false;
  for (const [key, { el, box }] of after) {
    const old = before.get(key);
    if (!old) {
      arriving.push(el);
      continue;
    }
    const v = speed.get(key)!;
    const axes: Axes = {
      x: [old.box.left - box.left, v.x[1]],
      y: [old.box.top - box.top, v.y[1]],
      w: [old.box.width - box.width, v.w[1]],
    };
    const shifted = Math.abs(axes.x[0]) >= 0.5 || Math.abs(axes.y[0]) >= 0.5;
    const resized = Math.abs(axes.w[0]) >= 1 && el.dataset.flipResize !== undefined;
    const opened = !resized && axes.w[0] <= -1;
    if (!shifted && !resized && !opened) continue;
    moved = true;
    const layout = el.dataset.flipResize !== undefined || el.dataset.flipLayout !== undefined;
    settle(el, axes, (x, y, w) => ({
      ...(layout ? { left: `${x}px`, top: `${y}px` } : { translate: `${x}px ${y}px` }),
      ...(resized && { width: `${box.width + w}px` }),
      ...(opened && { clipPath: `inset(0 ${-w}px 0 0 round var(--r-surface))` }),
    }));
  }
  if (!arriving.length) return;
  // An arrival rises in on the stage's beat and curve.
  const tokens = getComputedStyle(document.documentElement);
  const arrivalMs = Number.parseFloat(tokens.getPropertyValue("--dur-stage"));
  const easing = tokens.getPropertyValue("--ease-settle").trim();
  if (!(arrivalMs > ROOM_MS) || !easing) return;
  for (const el of arriving)
    el.animate(
      [
        { opacity: 0, translate: "0 8px" },
        { opacity: 1, translate: "0 0" },
      ],
      { duration: arrivalMs - ROOM_MS, delay: moved ? ROOM_MS : 0, easing, fill: "backwards" },
    );
}
