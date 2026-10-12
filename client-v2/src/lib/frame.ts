// The one frame clock: everything that moves per frame paints from a single requestAnimationFrame loop, which runs
// only while something moves and the page is visible.

/** Monotonic page milliseconds, the timeline frame timestamps share. */
export const pageMs = (): number => performance.now();

/** Paints one frame at `now` and returns whether it still moves. */
export type Painter = (now: number) => boolean;

const painters = new Set<Painter>();
/** This frame's painters, in one array reused frame to frame. */
const order: Painter[] = [];
let pending = 0;
let watching = false;

const visible = () => typeof document === "undefined" || !document.hidden;

function request(): void {
  if (!pending && painters.size && visible()) pending = globalThis.requestAnimationFrame?.(frame) ?? 0;
}

function frame(now: number): void {
  pending = 0;
  // A painter added during this frame starts with the next one; one that throws stops alone.
  order.length = 0;
  for (const paint of painters) order.push(paint);
  for (const paint of order) {
    if (!painters.has(paint)) continue;
    let moving = false;
    try {
      moving = paint(now);
    } catch (error) {
      reportError(error);
    }
    if (!moving) painters.delete(paint);
  }
  if (painters.size) request();
  else release();
}

function release(): void {
  if (pending) globalThis.cancelAnimationFrame?.(pending);
  pending = 0;
  if (watching) document.removeEventListener("visibilitychange", wake);
  watching = false;
}

// Painters stay registered while the page is hidden and resume from their own clocks when it shows again.
function wake(): void {
  if (visible()) request();
  else if (pending) {
    globalThis.cancelAnimationFrame?.(pending);
    pending = 0;
  }
}

/** Paints with `paint` from the next frame until it returns false or the returned stop runs. */
export function animate(paint: Painter): () => void {
  painters.add(paint);
  if (!watching && typeof document !== "undefined") {
    document.addEventListener("visibilitychange", wake);
    watching = true;
  }
  request();
  return () => {
    painters.delete(paint);
    if (!painters.size) release();
  };
}

/** Runs `fn` once on the next frame. */
export const nextFrame = (fn: (now: number) => void) =>
  animate((now) => {
    fn(now);
    return false;
  });
