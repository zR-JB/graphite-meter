// The one frame clock: everything that moves per frame paints from a single requestAnimationFrame loop, which
// runs only while something still moves.

/** Paints one frame at `now` and returns whether it still moves. */
export type Painter = (now: number) => boolean;

const painters = new Set<Painter>();
let pending = 0;

function frame(now: number): void {
  pending = 0;
  for (const paint of painters) if (!paint(now)) painters.delete(paint);
  if (painters.size) pending = requestAnimationFrame(frame);
}

/** Paints with `paint` from the next frame until it returns false or the returned stop runs. */
export function animate(paint: Painter): () => void {
  painters.add(paint);
  pending ||= requestAnimationFrame(frame);
  return () => void painters.delete(paint);
}
