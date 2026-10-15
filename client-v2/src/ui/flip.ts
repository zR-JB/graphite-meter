// Layout moves. A change that reshapes the console applies at once, in the frame of the input that asked for it;
// then every element marked `data-flip` that stayed springs from where it is seen to its new place on the
// compositor. One also marked `data-flip-resize` changes width on the same spring (a layout per frame within it); it,
// and one marked `data-flip-layout`, moves by relative offsets in layout, so a width and every surface that moves with
// it are drawn in the same frame: Firefox runs a translate on its compositor apart from those frames, and would draw
// a move that replaces another a frame off its place. Any other element that got wider opens from its old width. An
// arriving element waits for its neighbours to make room, then rises in; a leaving one goes at once. A move that
// interrupts another keeps its speed. A docked panel stays displayed beside its closed column, so its slide is an
// ordinary move in every engine.
import { reducedMotion, spring, SPRING_MS } from "../lib/motion";

type Axis = [offset: number, velocity: number];
type Axes = { x: Axis; y: Axis; w: Axis };

const ARRIVAL_MS = 380;
const ARRIVAL_EASE = "cubic-bezier(0.22, 1.2, 0.36, 1)";
/** Neighbours have moved most of the way by then, so an arriving element appears into room already made. */
const ROOM_MS = 140;
const STEPS = 30;

const moving = new WeakMap<Element, { axes: Axes; run: Animation }>();
let flipping = false;

/** The speed of `el`'s move, at its animation's own time: moves started together read the same instant, and a move
    still waiting for its first frame keeps its starting speed. */
const velocity = (el: Element): Axes => {
  const move = moving.get(el);
  const t = move ? Number(move.run.currentTime ?? SPRING_MS) : SPRING_MS;
  const v = (axis: keyof Axes): Axis => [0, t < SPRING_MS ? spring(move!.axes[axis], t)[1] : 0];
  return { x: v("x"), y: v("y"), w: v("w") };
};

/** Springs `el` from the given offsets to rest, drawn as 31 keyframes over 420 ms. */
function settle(el: Element, axes: Axes, frame: (x: number, y: number, w: number) => Keyframe): void {
  const frames = Array.from({ length: STEPS + 1 }, (_, i) => {
    const t = (i / STEPS) * SPRING_MS;
    const at = (axis: Axis) => (i === STEPS ? 0 : spring(axis, t)[0]);
    return frame(at(axes.x), at(axes.y), at(axes.w));
  });
  moving.set(el, { axes, run: el.animate(frames, { duration: SPRING_MS }) });
}

/** The displayed elements marked `data-flip`, by key. */
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

/** Applies `update` and moves what it moved; `only` names the keys that move, so a surface moves as one. A flip
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
  for (const el of arriving)
    el.animate(
      [
        { opacity: 0, translate: "0 8px" },
        { opacity: 1, translate: "0 0" },
      ],
      { duration: ARRIVAL_MS - ROOM_MS, delay: moved ? ROOM_MS : 0, easing: ARRIVAL_EASE, fill: "backwards" },
    );
}
